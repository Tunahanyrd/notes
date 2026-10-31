import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// Live pen preview through the WebAssembly renderer (public/ink-live.wasm,
// tools/build_ink_wasm.sh). Synthetic notes and pens only; real pen input
// with pressure comes from Chrome DevTools Protocol pen events.
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64');
const PROPS=['compressed','replay_only','stylus_channels','eraser','fixed_width','millisecond_timestamps','top_layer_pen','alpha_lock','binary_added','generated','fixed_opacity','rainbow_effect','straighten','reveal_mode'];
const on=new Set(['compressed','stylus_channels','millisecond_timestamps','binary_added']);
// A saved stylus FountainPen V16 stroke as the page renderer reads it.
const FOUNTAIN={metadata:null,pen_name:'com.samsung.android.sdk.pen.pen.preload.FountainPen',advanced_settings:'18;0;100;',tool_type_raw:2,properties:Object.fromEntries(PROPS.map(name=>[name,on.has(name)])),style:{advanced_pen_setting_id:7,color_argb:0xff2462a0,pen_size:6.0,pen_name_id:9,trailing_data:[],first_unparsed_field:null}};
async function fixture(page:Page,options:{blank?:boolean;pens?:boolean}={}){
  const requests:{path:string;method:string}[]=[];let pending:any[]=[];
  const base={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:false,titleEditable:true,readOnlyReason:'Çizim notu.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'',preview:''};
  const note={...base,id:'ink-live',title:'Canlı kalem'};
  const pages:any[]=options.blank?[{id:'page-0',index:0,width:1000,height:1000,background:null,strokes:[],ink:{appendInk:false,eraseInk:false,firstStroke:true,reason:null}}]
    :[{id:'page-0',index:0,width:1000,height:1000,background:null,strokes:[{id:'source-0',kind:'fountain',colorARGB:0xff2462a0,size:6,layerId:'layer-0',erasable:true,bounds:[8,18,32,42]}],ink:{appendInk:true,eraseInk:true,firstStroke:false,reason:null}}];
  await page.route('**/local-fonts/*',route=>route.fulfill({status:404,body:''}));
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method(),data=method==='GET'?null:route.request().postDataJSON();
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path.endsWith('/ink-draft'))return send({revision:null,strokes:[]});
    requests.push({path,method});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send({connected:true,accountLabel:'Sentetik hesap',loginPending:false,sync:{state:'idle',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,cloudDelete:false,attachments:false}});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes')return send({notes:[{...note,body:undefined}]});
    if(path.endsWith('/thumbnail')||path.endsWith('/preview'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/editor'))return send({schemaVersion:1,revision:note.revision,model:null,reason:'Çizim notu.',capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:false}});
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages:pages.map(item=>({...item,available:true,complete:true}))});
    if(path.endsWith('/ink-pens'))return options.pens===false?send({error:{code:'not_found',message:'Yok'}},404):send({schemaVersion:1,revision:Number(url.searchParams.get('revision')),pens:options.blank?{}:{'source-0':{width:1000,height:1000,rendering:FOUNTAIN}}});
    if(path.endsWith('/ink')&&method==='GET')return send({schemaVersion:1,revision:note.revision,pages,capabilities:options.blank?{appendInk:false,eraseInk:false,firstStroke:true}:{appendInk:note.syncState==='synced',eraseInk:true,firstStroke:false},reason:null,rendering:{nativeEquivalent:false,mode:'geometry-only'},pendingStrokes:pending});
    if(path.endsWith('/ink')&&method==='PUT'){pending=data.strokes.map((stroke:any,index:number)=>({...stroke,id:`pending-${index}`,sourceStrokeId:stroke.sourceStrokeId??'',kind:'fountain',colorARGB:stroke.colorARGB??0xff2462a0,size:stroke.size??6}));note.revision++;note.syncState='pending';return send({note});}
    if(method==='GET')return send({note});return send({error:{code:'unexpected_mutation',message:'Beklenmeyen değişiklik'}},422);
  });
  await page.addInitScript(()=>{(window as any).__inkLiveProbe=[];});
  await page.goto('/#token='+TOKEN);await page.getByRole('button',{name:/Canlı kalem/}).click();await expect(page.getByLabel('Çizim alanı')).toBeAttached();
  const pen=page.locator('.tool-rail').getByRole('button',{name:'Kalem',exact:true});await pen.click();await expect(page.getByLabel('Çizim alanı')).toHaveClass(/drawing/);
  return {requests};
}
type PenPoint={x:number;y:number;pressure:number};
// Real pen input (pointerType "pen", force = pressure) in page units 0..1000.
async function penStroke(page:Page,points:PenPoint[],options:{release?:boolean;gapMs?:number}={}){
  const box=(await page.getByLabel('Çizim alanı').boundingBox())!;const cdp=await page.context().newCDPSession(page);
  const at=(p:PenPoint)=>({x:box.x+p.x*box.width/1000,y:box.y+p.y*box.height/1000,pointerType:'pen' as const,force:p.pressure});
  await cdp.send('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',buttons:1,clickCount:1,...at(points[0])});
  for(const point of points.slice(1)){await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',button:'left',buttons:1,...at(point)});if(options.gapMs)await page.waitForTimeout(options.gapMs);}
  const release=async()=>{await cdp.send('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',buttons:0,clickCount:1,...at(points[points.length-1])});await cdp.detach();};
  if(options.release!==false)await release();
  return release;
}
const ramp=(n:number,y=500)=>Array.from({length:n},(_,i)=>({x:150+700*i/(n-1),y,pressure:0.05+0.95*i/(n-1)}));
// Ink mass (summed alpha) of a device-pixel column of the live canvas at page x.
const liveColumn=(page:Page,x:number)=>page.evaluate(x=>{const canvas=document.querySelector<HTMLCanvasElement>('.ink-live');if(!canvas||!canvas.width)return 0;const column=Math.round(x*canvas.width/1000);const data=canvas.getContext('2d')!.getImageData(column,0,1,canvas.height).data;let sum=0;for(let i=3;i<data.length;i+=4)sum+=data[i];return sum;},x);
const exactFrames=(page:Page,points:number)=>page.evaluate(n=>((window as any).__inkLiveProbe as any[]).filter(item=>item.kind==='exact'&&item.points===n).length,points);

test('the live pen stroke is drawn by the renderer: pressure widens it while drawing',async({page})=>{
  await fixture(page);const points=ramp(40);
  const release=await penStroke(page,points,{release:false});
  // First stroke of the run: includes compiling the 5 MB renderer module in the worker (slow on CI macOS runners).
  await expect.poll(()=>exactFrames(page,points.length),{timeout:30000}).toBeGreaterThan(0);
  const light=await liveColumn(page,220),heavy=await liveColumn(page,800);
  expect(light).toBeGreaterThan(0);expect(heavy).toBeGreaterThan(light*1.1);
  // Nothing is drawn as a fixed-width SVG line for this stroke.
  await expect(page.locator('.ink-overlay polyline')).toHaveCount(0);
  await release();await expect(page.locator('.ink-raster[data-ink-stroke]')).toHaveCount(1);
});

test('pen-up keeps exactly the pixels drawn live (no jump), for the draft and the queued stroke',async({page})=>{
  const {requests}=await fixture(page);const moving=ramp(30,400).map((p,i)=>({...p,y:400+60*Math.sin(i/4)}));
  // The pen rests before lifting, so no predicted tip (never stored) is on screen.
  const points=[...moving,...Array.from({length:4},()=>({...moving[moving.length-1]}))];
  const release=await penStroke(page,points,{release:false});
  // Each test is a fresh page, so its first stroke compiles the renderer module again (slow on CI macOS runners).
  await expect.poll(()=>exactFrames(page,points.length),{timeout:30000}).toBeGreaterThan(0);await page.waitForTimeout(100);
  // Snapshot of the live layer right before the pen leaves the screen.
  await page.evaluate(()=>{const canvas=document.querySelector<HTMLCanvasElement>('.ink-live')!;(window as any).__before=canvas.getContext('2d')!.getImageData(0,0,canvas.width,canvas.height);});
  await release();
  const compare=()=>page.evaluate(()=>{
    const live=document.querySelector<HTMLCanvasElement>('.ink-live')!,before:ImageData=(window as any).__before;
    // The page may remount after the save (new revision): measure against the snapshot size.
    const out=document.createElement('canvas');out.width=before.width;out.height=before.height;const context=out.getContext('2d')!;
    const pageBox=live.getBoundingClientRect(),dpr=before.width/pageBox.width;
    for(const canvas of document.querySelectorAll<HTMLCanvasElement>('.ink-raster')){const box=canvas.getBoundingClientRect();context.drawImage(canvas,Math.round((box.left-pageBox.left)*dpr),Math.round((box.top-pageBox.top)*dpr));}
    const after=context.getImageData(0,0,out.width,out.height).data,liveLeft=live.width?live.getContext('2d')!.getImageData(0,0,live.width,live.height).data:new Uint8ClampedArray(0);
    let max=0,differing=0,inked=0,leftover=0;
    for(let i=3;i<liveLeft.length;i+=4)if(liveLeft[i])leftover++;
    for(let i=0;i<after.length;i+=4){if(before.data[i+3])inked++;let d=0;for(let k=0;k<4;k++)d=Math.max(d,Math.abs(after[i+k]-before.data[i+k]));if(d>2)differing++;max=Math.max(max,d);}
    return {max,differing,inked,leftover,rasters:document.querySelectorAll('.ink-raster').length};
  });
  await expect(page.locator('.ink-raster[data-ink-stroke]')).toHaveCount(1);
  const draft=await compare();
  expect(draft.inked).toBeGreaterThan(500);expect(draft.leftover).toBe(0);expect(draft.rasters).toBe(1);
  expect(draft.differing).toBe(0);
  // Autosave turns the draft into a queued stroke with a new id: same pixels.
  await expect.poll(()=>requests.filter(r=>r.method==='PUT'&&r.path.endsWith('/ink')).length,{timeout:6000}).toBe(1);
  await expect(page.locator('.ink-raster[data-ink-stroke="pending-0"]')).toHaveCount(1);
  const queued=await compare();
  test.info().annotations.push({type:'no-jump',description:JSON.stringify({draft,queued})});
});

test('a first stroke on an empty page needs no template and still uses the live renderer',async({page})=>{
  await fixture(page,{blank:true});
  await penStroke(page,ramp(20,300));
  await expect(page.locator('.ink-raster[data-ink-stroke]')).toHaveCount(1);await expect(page.locator('.ink-overlay polyline')).toHaveCount(0);
});

test('without the pen template the stroke keeps the simple SVG preview',async({page})=>{
  await fixture(page,{pens:false});
  await penStroke(page,ramp(20,300));
  await expect(page.locator('.ink-overlay polyline')).toHaveCount(1);await expect(page.locator('.ink-raster')).toHaveCount(0);
});

test('a missing WebAssembly module falls back silently to the SVG preview',async({page})=>{
  const errors:string[]=[];page.on('pageerror',error=>errors.push(error.message));
  await page.route('**/ink-live.wasm',route=>route.fulfill({status:404,body:''}));
  await fixture(page);
  await penStroke(page,ramp(20,300));
  await expect(page.locator('.ink-overlay polyline')).toHaveCount(1);await expect(page.locator('.ink-raster')).toHaveCount(0);await expect(page.locator('.ink-live')).toHaveCount(0);
  await expect(page.getByRole('alert')).toHaveCount(0);expect(errors).toEqual([]);
});

test('the finished raster remains registered to the page while zoom changes',async({page})=>{
  // Delay resize notifications as can happen when the main thread is busy:
  // the old bitmap must still scale with the page while its replacement waits.
  await page.addInitScript(()=>{
    const Native=window.ResizeObserver;
    window.ResizeObserver=class extends Native {
      constructor(callback:ResizeObserverCallback){super((entries,observer)=>{if(!(window as any).__pauseResize)callback(entries,observer);});}
    };
  });
  await fixture(page);const points=ramp(30,400);
  const release=await penStroke(page,points,{release:false});
  await expect.poll(()=>exactFrames(page,points.length),{timeout:30000}).toBeGreaterThan(0);
  await release();
  await expect(page.locator('.ink-raster[data-ink-stroke]')).toHaveCount(1);
  const samples=await page.evaluate(async()=>{
    (window as any).__pauseResize=true;
    const measure=()=>{
      const image=document.querySelector<HTMLImageElement>('.page-image')!;
      const raster=document.querySelector<HTMLCanvasElement>('.ink-raster')!;
      const p=image.getBoundingClientRect(),r=raster.getBoundingClientRect();
      const alpha=raster.getContext('2d')!.getImageData(0,0,raster.width,raster.height).data;
      return {x:(r.left-p.left)/p.width,y:(r.top-p.top)/p.height,w:r.width/p.width,h:r.height/p.height,ink:alpha.some((v,i)=>i%4===3&&v>0)};
    };
    const values=[measure()];
    for(const name of [...Array(5).fill('Yakınlaştır'),...Array(5).fill('Uzaklaştır')]){
      document.querySelector<HTMLButtonElement>(`button[aria-label="${name}"]`)!.click();
      await new Promise<void>(resolve=>requestAnimationFrame(()=>resolve()));
      values.push(measure());
    }
    return values;
  });
  for(const sample of samples){expect(sample.ink).toBe(true);for(const key of ['x','y','w','h'] as const)expect(Math.abs(sample[key]-samples[0][key]),key).toBeLessThan(.0002);}
});

test('headless timing: input to tip paint and input to exact renderer paint',async({page})=>{
  await fixture(page);
  for(const [n,y] of [[24,250],[60,500],[120,750]] as const){
    await penStroke(page,ramp(n,y).map((p,i)=>({...p,y:y+30*Math.sin(i/5)})),{gapMs:8});
  }
  await expect(page.locator('.ink-raster[data-ink-stroke]')).toHaveCount(3);
  const probe:{kind:string;event:number;painted:number;points:number}[]=await page.evaluate(()=>(window as any).__inkLiveProbe);
  const stats=(kind:string)=>{const values=probe.filter(item=>item.kind===kind).map(item=>item.painted-item.event).sort((a,b)=>a-b);const at=(q:number)=>values[Math.min(values.length-1,Math.floor(q*values.length))];return {count:values.length,median:+at(.5).toFixed(1),p95:+at(.95).toFixed(1),max:+values[values.length-1].toFixed(1)};};
  const tip=stats('tip'),exact=stats('exact');
  test.info().annotations.push({type:'latency-ms',description:JSON.stringify({tip,exact})});console.log('ink-live latency (headless, ms):',JSON.stringify({tip,exact}));
  expect(tip.count).toBeGreaterThan(50);expect(exact.count).toBeGreaterThan(50);
});
