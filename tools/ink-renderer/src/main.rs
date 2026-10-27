//! GPL-3.0 bounded adapter to pinned sdocx. SVG output contains private geometry.
use std::io::{Read,Write};
use serde::Deserialize;
use sdocx::{BoundingBox,Color,Document,DocumentMetadata,InkPen2RenderProfile,InkSupport,Page,RenderOptions,RenderTheme,RichTextBox,RichTextParagraph,RichTextParagraphType,RichTextSection,RichTextSpan,RichTextSpanType,SpanIntervalType,Stroke,TextAreaType};
const MAX_INPUT:u64=24*1024*1024;
const MAX_OUTPUT:usize=16*1024*1024;
/// Saved strokes may run slightly past the page edge (observed up to ~70
/// units); the page viewport clips them. Farther geometry stays rejected.
const EDGE:f64=512.;
const MAX_FLOW_TEXT:usize=256*1024;
const MAX_FLOW_PAGES:usize=4096;
const FOUNTAIN:&str="com.samsung.android.sdk.pen.pen.preload.FountainPen";
const INKPEN2:&str="com.samsung.android.sdk.pen.pen.preload.InkPen2";
const MARKER4:&str="com.samsung.android.sdk.pen.pen.preload.Marker4";
#[derive(Deserialize)]
#[serde(deny_unknown_fields,rename_all="camelCase")]
struct Input {schema_version:u32,width:u32,height:u32,background_argb:u32,#[serde(default)] inkpen2_profile:Option<Profile>,#[serde(default)] admit:Option<String>,#[serde(default)] flow:Option<Flow>,strokes:Vec<Stroke>}
/// Caller-declared InkPen2 V9 host profile; upstream never infers device state.
#[derive(Deserialize)]
#[serde(deny_unknown_fields,rename_all="camelCase")]
struct Profile {screen_width:u32,screen_height:u32,enhanced_antialias:bool}
/// Note-level body text flowing over a page-list note (Samsung "document
/// Flow"). Laid out by upstream's own body pipeline with its pinned fonts;
/// only plain style spans and alignment/parsing paragraphs are accepted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields,rename_all="camelCase")]
struct Flow {
    text:String,spans:Vec<SpanIn>,paragraphs:Vec<ParagraphIn>,
    /// Saved per-page UTF-16 text sections (start,length); (-1,0) is absent.
    /// Empty requests upstream's measured full-source reflow instead.
    sections:Vec<(i32,i32)>,
    margins:[f32;4],gravity:u8,
    pages:Vec<PageIn>,page_index:usize,
    padding:(u32,u32),font_size_delta:Option<i32>,default_page:(u32,u32),flow_size:(u32,u32),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields,rename_all="camelCase")]
struct SpanIn {kind:u32,start:u32,end:u32,interval:u32,payload:Vec<u8>}
#[derive(Deserialize)]
#[serde(deny_unknown_fields,rename_all="camelCase")]
struct ParagraphIn {kind:u32,start:u32,end:u32,payload:Vec<u8>}
#[derive(Deserialize)]
#[serde(deny_unknown_fields,rename_all="camelCase")]
struct PageIn {width:u32,height:u32,background_argb:u32}
fn rgb(argb:u32)->Color{Color{r:(argb>>16) as u8,g:(argb>>8) as u8,b:argb as u8}}
fn page_bounds(width:u32,height:u32)->bool{width>0&&height>0&&width<=32768&&height<=32768&&u64::from(width)*u64::from(height)<=4*1024*1024}
/// Strict conversion into upstream's body model; anything outside the
/// admitted span/paragraph kinds rejects the page instead of being dropped.
fn body_text(flow:&Flow)->Result<RichTextBox,&'static str>{
    let length=flow.text.encode_utf16().count();
    if flow.text.is_empty()||flow.text.len()>MAX_FLOW_TEXT||flow.spans.len()+flow.paragraphs.len()>8192{return Err("unsupported body bounds");}
    if flow.margins.iter().any(|m|!m.is_finite()||!(0. ..=1000.).contains(m))||flow.gravity!=0{return Err("unsupported body frame");}
    let mut spans=Vec::with_capacity(flow.spans.len());
    for s in &flow.spans{
        let kind=RichTextSpanType::from(s.kind);
        let size=match kind{RichTextSpanType::Bold|RichTextSpanType::Italic|RichTextSpanType::Underline|RichTextSpanType::Strikethrough|RichTextSpanType::ForegroundColor|RichTextSpanType::BackgroundColor|RichTextSpanType::FontSize=>4,_=>return Err("unsupported body span")};
        if s.payload.len()!=size||s.start>s.end||s.end as usize>length||s.interval>3{return Err("invalid body span");}
        if kind==RichTextSpanType::FontSize{let v=f32::from_le_bytes(s.payload[..4].try_into().unwrap());if !v.is_finite()||!(0. ..=1000.).contains(&v){return Err("invalid body span");}}
        spans.push(RichTextSpan{kind,start_utf16:s.start,end_utf16:s.end,interval_type:SpanIntervalType::from(s.interval),payload:s.payload.clone()});
    }
    let mut paragraphs=Vec::with_capacity(flow.paragraphs.len());
    for p in &flow.paragraphs{
        let kind=RichTextParagraphType::from(p.kind);
        if !matches!(kind,RichTextParagraphType::Alignment|RichTextParagraphType::ParsingState)||p.payload.len()!=4||p.start>=p.end||p.end as usize>length+1{return Err("unsupported body paragraph");}
        if kind==RichTextParagraphType::Alignment&&u32::from_le_bytes(p.payload[..4].try_into().unwrap())>3{return Err("unsupported body paragraph");}
        paragraphs.push(RichTextParagraph{kind,start_paragraph:p.start,end_paragraph:p.end,payload:p.payload.clone()});
    }
    let mut sections=Vec::with_capacity(flow.sections.len());
    for &(start,count) in &flow.sections{
        let valid=(start==-1&&count==0)||(start>=0&&count>=0&&(start as i64+count as i64)<=length as i64);
        if !valid{return Err("invalid body section");}
        sections.push(RichTextSection{start_utf16:start,length_utf16:count});
    }
    if !sections.is_empty()&&sections.len()!=flow.pages.len(){return Err("invalid body section");}
    Ok(RichTextBox{text_area_type:Some(TextAreaType::Margin),bbox:BoundingBox::default(),rotation_degrees:None,text:flow.text.clone(),color:None,highlight_color:None,underline:false,font_size:None,runs:Vec::new(),spans,paragraphs,object_spans:Vec::new(),text_sections:sections,margins:Some(flow.margins),gravity:Some(flow.gravity)})
}
fn render(mut input:Input)->Result<String,&'static str>{
    // "native" (default): native-derived routes only. "approximate": also accept
    // upstream's documented InkPen2 approximation. "v9ms": InkPen2 9; strokes with
    // millisecond timestamps / saved tolerance are fed to the V9 route as if
    // those were unset (calibration experiment against Samsung caches).
    let admit=input.admit.clone().unwrap_or_else(||"native".into());
    if !matches!(admit.as_str(),"native"|"approximate"|"v9ms"|"calibrated"){return Err("unsupported admission mode");}
    // "calibrated": 9; strokes take the relaxed V9 route, other InkPen2 the
    // documented approximation; matched Samsung light caches (ink-renderer-v2).
    if admit=="v9ms"||admit=="calibrated"{for stroke in &mut input.strokes{if let Some(r)=stroke.rendering.as_mut(){if r.pen_name.as_deref()==Some(INKPEN2)&&r.advanced_settings.as_deref()==Some("9;"){r.properties.millisecond_timestamps=false;r.style.initial_tolerance=Some(0.0);}}}}
    // Highlighter: Samsung's GLV8 redraw (libSPenMarker4.so 0x54b2c) only
    // singles out tool 0; every other tool, mouse 3 included, takes the stylus
    // path, which upstream models as tool 2.
    for stroke in &mut input.strokes{if let Some(r)=stroke.rendering.as_mut(){if r.pen_name.as_deref()==Some(MARKER4)&&r.tool_type_raw==3{r.tool_type_raw=2;}}}
    // A body-text page may legitimately carry no strokes of its own.
    if input.schema_version!=1||!page_bounds(input.width,input.height)||input.background_argb>>24!=255||(input.strokes.is_empty()&&input.flow.is_none())||input.strokes.len()>1024{return Err("unsupported input bounds");}
    let profile=match &input.inkpen2_profile{None=>None,Some(p) if (1..=32768).contains(&p.screen_width)&&(1..=32768).contains(&p.screen_height)=>Some(InkPen2RenderProfile{screen_width:p.screen_width,screen_height:p.screen_height,enhanced_antialias:p.enhanced_antialias}),_=>return Err("unsupported pen profile")};
    let mut count=0;let mut distance=0.;
    for stroke in &input.strokes{
        count+=stroke.points.len();
        if stroke.points.len()<2||count>200_000||stroke.pressures.len()!=stroke.points.len()||stroke.timestamps.len()!=stroke.points.len(){return Err("unsupported sample inventory");}
        let r=stroke.rendering.as_ref().ok_or("saved rendering inputs required")?;
        let fountain=r.pen_name.as_deref()==Some(FOUNTAIN)&&matches!(r.advanced_settings.as_deref(),Some("14;"|"18;0;100;"));
        let inkpen2=r.pen_name.as_deref()==Some(INKPEN2)&&(admit=="approximate"||admit=="calibrated"||r.advanced_settings.as_deref()==Some("9;"))&&profile.is_some();
        // Marker4 is upstream's documented rounded-rectangle approximation,
        // accepted only in the calibrated (approximation-allowing) mode.
        let marker4=r.pen_name.as_deref()==Some(MARKER4)&&matches!(r.advanced_settings.as_deref(),Some("7;"|"8;"))&&admit=="calibrated";
        if !(fountain||inkpen2||marker4)||r.metadata.is_some()||r.style.first_unparsed_field.is_some()||!r.style.trailing_data.is_empty(){return Err("unsupported brush profile");}
        let argb=r.style.color_argb.ok_or("source color required")?;
        if stroke.color!=Some(rgb(argb))||r.style.pen_size!=Some(stroke.pen_width){return Err("source style mismatch");}
        if stroke.points.iter().any(|p|!p.x.is_finite()||!p.y.is_finite()||p.x< -EDGE||p.y< -EDGE||p.x>input.width as f64+EDGE||p.y>input.height as f64+EDGE)||stroke.pressures.iter().any(|p|!p.is_finite()||!(0. ..=1.).contains(p)){return Err("unsupported sample range");}
        for pair in stroke.points.windows(2){distance+=(pair[0].x-pair[1].x).hypot(pair[0].y-pair[1].y);}
        if !distance.is_finite()||distance>500_000.{return Err("geometry budget exceeded");}
    }
    let page_of=|uuid:String,width:u32,height:u32,argb:u32,strokes:Vec<Stroke>|Page{uuid,width,height,content_bbox:BoundingBox::default(),background_color:Some(rgb(argb)),template:None,background:Default::default(),objects:strokes.into_iter().map(Into::into).collect()};
    let strokes=std::mem::take(&mut input.strokes);
    let (doc,page_index)=match input.flow.take(){
        None=>{
            let metadata=DocumentMetadata{page_ids:vec!["page".into()],page_dimensions:Some((input.width,input.height)),dark_mode_compatibility:Some(false),..Default::default()};
            (Document{pages:vec![page_of("page".into(),input.width,input.height,input.background_argb,strokes)],metadata},0)
        }
        Some(flow)=>{
            // Every physical page takes part in body pagination; only the
            // requested page carries its strokes.
            let n=flow.pages.len();
            if n==0||n>MAX_FLOW_PAGES||flow.page_index>=n{return Err("invalid flow pages");}
            let own=&flow.pages[flow.page_index];
            if (own.width,own.height,own.background_argb)!=(input.width,input.height,input.background_argb){return Err("invalid flow pages");}
            if flow.pages.iter().any(|p|!page_bounds(p.width,p.height)||p.background_argb>>24!=255){return Err("invalid flow pages");}
            let total=flow.pages.iter().map(|p|u64::from(p.height)).sum::<u64>()+(n as u64-1)*u64::from(flow.padding.1);
            if flow.padding.0>32768||flow.padding.1>32768||total!=u64::from(flow.flow_size.1)||flow.flow_size.0==0||!page_bounds(flow.default_page.0,flow.default_page.1.min(32768))||flow.default_page.1==0{return Err("invalid flow geometry");}
            if flow.font_size_delta.is_some_and(|d|d!=i32::MIN&&!(-100..=100).contains(&d)){return Err("invalid flow font size");}
            let note_text=body_text(&flow)?;
            let mut strokes=Some(strokes);
            let pages=flow.pages.iter().enumerate().map(|(i,p)|page_of(format!("page{i}"),p.width,p.height,p.background_argb,if i==flow.page_index{strokes.take().unwrap_or_default()}else{Vec::new()})).collect::<Vec<_>>();
            let metadata=DocumentMetadata{page_ids:pages.iter().map(|p|p.uuid.clone()).collect(),page_dimensions:Some((flow.pages[0].width,flow.pages[0].height)),default_page_dimensions:Some(flow.default_page),page_mode:Some(0),orientation:Some(0),body_font_size_delta:flow.font_size_delta,flow_dimensions:Some(flow.flow_size),flow_page_padding:Some(flow.padding),dark_mode_compatibility:Some(false),note_text:Some(note_text),..Default::default()};
            (Document{pages,metadata},flow.page_index)
        }
    };
    let mut options=RenderOptions::default();options.inkpen2_profile=profile;
    let page=&doc.pages[page_index];
    let theme=RenderTheme::resolve(page,&doc.metadata,options.color_mode);
    let mut stamps=0;
    for stroke in page.strokes(){
        // Same preparation the SVG route selects. Only native-derived records
        // count: an upstream generic fallback rejects the whole page.
        let prepared=sdocx::prepare_stroke_for_page_with_theme(stroke,theme,&options);
        let native=match prepared.profile{Some("FountainPen")=>prepared.dot_radii.is_some()&&prepared.inkpen2_v9.is_none(),Some("InkPen2V9")=>prepared.inkpen2_v9.as_ref().is_some_and(|ink|!ink.per_stamp_rgb&&ink.records.len()==prepared.points.len()),_=>false};
        let is_marker4=stroke.rendering.as_ref().is_some_and(|r|r.pen_name.as_deref()==Some(MARKER4));
        // A Marker4 stroke counts only when upstream's Marker4 tip path ran
        // (rect stamp), never its generic vector fallback.
        let approximate_ok=(admit=="approximate"||(admit=="calibrated"&&prepared.profile!=Some("FountainPen")))&&matches!(prepared.support,InkSupport::Approximate)&&(!is_marker4||prepared.rect_stamp.is_some());
        if !approximate_ok&&(!matches!(prepared.support,InkSupport::Reconstructed)||!native){return Err("brush reconstruction unavailable");}
        stamps+=prepared.points.len();if stamps>2_000_000{return Err("stamp budget exceeded");}
    }
    let layout=sdocx::layout_document(&doc);
    let position=layout.pages.iter().position(|p|p.source_page_index==page_index).ok_or("page not visible")?;
    if doc.metadata.note_text.is_some(){
        // A body slice whose authoritative source cannot be rebuilt would be
        // skipped silently upstream; refuse it instead.
        let lp=&layout.pages[position];
        if lp.body_text.is_some()&&lp.body_text_capture(&doc).is_none()&&lp.body_text_reflow(&doc).is_none(){return Err("body source unavailable");}
    }
    let output=sdocx::render_layout_page_svg_with_fonts(&doc,&layout,position,&options,&sdocx::fonts::FontBook::default()).ok_or("render unavailable")?;
    if !output.text_diagnostics.is_empty()||!output.object_diagnostics.is_empty()||!output.geometry_diagnostics.is_empty()||!output.paint_diagnostics.is_empty()||!output.background_diagnostics.is_empty(){return Err("incomplete rendering");}
    let svg=if doc.metadata.note_text.is_some(){outline_text(output.svg)?}else{output.svg};
    if svg.len()>MAX_OUTPUT||svg.contains("<image")||svg.contains("<script"){return Err("invalid render output");}
    Ok(svg)
}
/// Body text arrives as SVG text in upstream's private embedded faces, which
/// the sandboxed rasterizer cannot load (it would substitute a system font
/// and ignore the measured glyph positions). Convert it to outlines with the
/// same pinned font database; an unresolvable face rejects the page.
fn outline_text(svg:String)->Result<String,&'static str>{
    if !svg.contains("<text"){return Ok(svg);}
    let book=sdocx::fonts::FontBook::default();
    let database=book.database();
    let faces=sdocx::fonts::SvgFontFamilies::new(&database);
    let mut rest=svg.as_str();
    while let Some(at)=rest.find("sdocx-face-"){
        let name=rest[at..].split(|c:char|!(c.is_ascii_alphanumeric()||c=='-')).next().unwrap_or("");
        if faces.resolve(name).is_none(){return Err("text face unavailable");}
        rest=&rest[at+name.len().max(1)..];
    }
    let mut options=usvg::Options::default();
    options.fontdb=database.clone();
    options.font_resolver=usvg::FontResolver{
        select_font:Box::new(move|font,_|match font.families().first(){Some(usvg::FontFamily::Named(name))=>faces.resolve(name),_=>None}),
        select_fallback:Box::new(|_,_,_|None),
    };
    let tree=usvg::Tree::from_str(&svg,&options).map_err(|_|"text outline unavailable")?;
    let out=tree.to_string(&usvg::WriteOptions::default());
    if out.contains("<text"){return Err("text outline unavailable");}
    Ok(out)
}
fn main(){
    let outcome=(||{
        let mut bytes=Vec::new();std::io::stdin().take(MAX_INPUT+1).read_to_end(&mut bytes).map_err(|_|"input unavailable")?;
        if bytes.len()>MAX_INPUT as usize{return Err("input too large");}
        let input=serde_json::from_slice(&bytes).map_err(|_|"invalid input")?;
        let svg=render(input)?;
        std::io::stdout().lock().write_all(svg.as_bytes()).map_err(|_|"output unavailable")
    })();
    if let Err(reason)=outcome{eprintln!("ink-render: {reason}");std::process::exit(2);}
}
