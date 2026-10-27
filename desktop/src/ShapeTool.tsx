import {useCallback, useEffect, useRef, useState, type ReactElement} from 'react';
import {Circle, Diamond, Minus, MoveUpRight, Palette, Square, Triangle} from 'lucide-react';
import {api, notePath} from './api';
import type {Note} from './types';
import {inkColor} from './InkEditor';
import './shape-tool.css';

// Shapes tool (Şekil): one shape per Cloud write (page-ops op 'shape').
// The backend writes Samsung shape/line objects; this file only draws them as
// SVG geometry (not Samsung's renderer) and collects a drag on a page.
export type ShapeKind='rectangle'|'ellipse'|'triangle'|'rightTriangle'|'diamond'|'line'|'arrow';
export interface ShapeItem {id:string;kind:ShapeKind;geometry:[number,number,number,number];colorARGB:number;width:number;fillARGB:number|null}
export interface ShapePage {id:string;index:number;width?:number;height?:number;addShape?:boolean;reason?:string|null;shapes:ShapeItem[]}
export interface ShapeProjection {schemaVersion:1;revision:number;kinds:ShapeKind[];pages:ShapePage[];reason:string|null}
export interface ShapeStyle {colorARGB:number;width:number;fill:boolean;fillARGB:number}

export const SHAPE_KINDS:{kind:ShapeKind;label:string}[]=[
  {kind:'rectangle',label:'Dikdörtgen'},{kind:'ellipse',label:'Elips'},{kind:'triangle',label:'Üçgen'},
  {kind:'rightTriangle',label:'Dik üçgen'},{kind:'diamond',label:'Eşkenar dörtgen'},{kind:'line',label:'Çizgi'},{kind:'arrow',label:'Ok'}];
export const MIN_SHAPE_WIDTH=0.5,MAX_SHAPE_WIDTH=64;
const LINE_KINDS:ShapeKind[]=['line','arrow'];
const shapeColors=['#252525','#ff343b','#ffbb24','#26ce62','#229bff','#254bff','#a826fa','#f382b4','#976c32','#706a77','#ffffff','#2bdacb'];
const argbOf=(hex:string,alpha=0xff)=>(((alpha<<24)|parseInt(hex.slice(1),16))>>>0);

export function useShapes(note:Note|null,enabled:boolean,blockedReason:string,onSaved:(note:Note)=>void){
  const [projection,setProjection]=useState<ShapeProjection|null>(null);
  const [active,setActive]=useState(false);const [kind,setKind]=useState<ShapeKind>('rectangle');
  const [style,setStyle]=useState<ShapeStyle>({colorARGB:0xff252525,width:4,fill:false,fillARGB:argbOf('#229bff',0x40)});
  const [saving,setSaving]=useState(false);const [error,setError]=useState('');
  const [pending,setPending]=useState<{pageId:string;shape:ShapeItem;revision:number}|null>(null);
  const noteId=note?.id??null,revision=note?.revision,syncState=note?.syncState;
  useEffect(()=>{
    if(!noteId||!enabled){setProjection(null);setActive(false);return;}
    const controller=new AbortController();
    void api<ShapeProjection>(notePath(noteId)+'/shapes','GET',undefined,controller.signal).then(value=>{
      if(controller.signal.aborted||value?.schemaVersion!==1||!Array.isArray(value.pages))return;
      setProjection(value);
      // The optimistic shape stays until the synced projection contains it.
      setPending(previous=>previous&&value.pages.some(page=>page.shapes.some(shape=>shape.id===previous.shape.id||
        (page.id===previous.pageId&&shape.kind===previous.shape.kind&&shape.geometry.every((v,i)=>Math.abs(v-previous.shape.geometry[i])<1))))?null:previous);
    }).catch(()=>{if(!controller.signal.aborted)setProjection(null);});
    return()=>controller.abort();
  },[noteId,revision,syncState,enabled]);
  useEffect(()=>{if(syncState&&syncState!=='pending'&&syncState!=='synced')setPending(null);},[syncState]);
  const page=(index:number)=>projection?.pages.find(item=>item.index===index);
  const reasonFor=(index:number):string=>{
    if(!projection)return 'Şekil bilgisi yükleniyor.';
    if(blockedReason)return blockedReason;
    if(saving||pending||syncState==='pending')return 'Önceki şekil eşitleniyor; bitince yenisini çizebilirsiniz.';
    if(syncState&&syncState!=='synced')return 'Not eşitlenmeden şekil eklenemez.';
    const current=page(index);
    if(!current)return projection.reason||'Bu sayfaya şekil eklenemiyor.';
    return current.addShape?'':(current.reason||projection.reason||'Bu sayfaya şekil eklenemiyor.');
  };
  const available=!!projection&&projection.pages.some(item=>item.addShape)&&!blockedReason;
  const submit=useCallback(async(pageId:string,geometry:[number,number,number,number])=>{
    if(!note||saving)return;
    const shape={kind,geometry,colorARGB:style.colorARGB,width:style.width,...(style.fill&&!LINE_KINDS.includes(kind)?{fillARGB:style.fillARGB}:{})};
    setSaving(true);setError('');
    try{
      const result=await api<{note:Note}>(notePath(note.id)+'/page-ops','POST',{op:'shape',pageId,shape,expectedRevision:note.revision});
      setPending({pageId,revision:result.note.revision,shape:{id:'pending',kind,geometry,colorARGB:style.colorARGB,width:style.width,fillARGB:shape.fillARGB??null}});
      onSaved(result.note);
    }catch(e){setError(e instanceof Error?e.message:'Şekil kaydedilemedi.');}
    finally{setSaving(false);}
  },[note,saving,kind,style,onSaved]);
  return {projection,active,setActive,kind,setKind,style,setStyle,saving,error,setError,pending,page,reasonFor,available,submit};
}
export type ShapeController=ReturnType<typeof useShapes>;

function ShapeSvg({shape,className}:{shape:Omit<ShapeItem,'id'>;className?:string}){
  const [x1,y1,x2,y2]=shape.geometry;const {color,opacity}=inkColor(shape.colorARGB);
  const fill=shape.fillARGB!=null?inkColor(shape.fillARGB):null;
  const common={className,stroke:color,strokeOpacity:opacity,strokeWidth:shape.width,fill:fill?.color??'none',fillOpacity:fill?.opacity??0,strokeLinejoin:'miter' as const};
  const l=Math.min(x1,x2),t=Math.min(y1,y2),r=Math.max(x1,x2),b=Math.max(y1,y2),cx=(l+r)/2,cy=(t+b)/2;
  switch(shape.kind){
    case 'rectangle':return <rect {...common} x={l} y={t} width={r-l} height={b-t}/>;
    case 'ellipse':return <ellipse {...common} cx={cx} cy={cy} rx={(r-l)/2} ry={(b-t)/2}/>;
    case 'triangle':return <polygon {...common} points={`${cx},${t} ${r},${b} ${l},${b}`}/>;
    case 'rightTriangle':return <polygon {...common} points={`${l},${t} ${r},${b} ${l},${b}`}/>;
    case 'diamond':return <polygon {...common} points={`${cx},${t} ${r},${cy} ${cx},${b} ${l},${cy}`}/>;
    default:{
      const head=[] as string[];
      if(shape.kind==='arrow'){const angle=Math.atan2(y2-y1,x2-x1),size=Math.max(8,shape.width*3);
        for(const side of [-1,1])head.push(`${x2-size*Math.cos(angle+side*Math.PI/7)},${y2-size*Math.sin(angle+side*Math.PI/7)}`);}
      return <g className={className}><line x1={x1} y1={y1} x2={x2} y2={y2} stroke={color} strokeOpacity={opacity} strokeWidth={shape.width}/>
        {head.length?<polygon points={`${x2},${y2} ${head.join(' ')}`} fill={color} fillOpacity={opacity}/>:null}</g>;
    }
  }
}

// Drawn over a page sheet. Captures pointer input only while the tool is on
// and this page accepts shapes; otherwise the pen/eraser layer underneath works.
export function ShapeLayer({controller,pageIndex,current}:{controller:ShapeController;pageIndex:number;current:boolean}){
  const page=controller.page(pageIndex);const svg=useRef<SVGSVGElement>(null);
  const [drag,setDrag]=useState<{pointer:number;start:[number,number];end:[number,number]}|null>(null);
  if(!page||!page.width||!page.height)return null;
  const width=page.width,height=page.height;
  const reason=controller.active?controller.reasonFor(pageIndex):'';
  const drawing=controller.active&&!reason;
  const pending=controller.pending&&controller.pending.pageId===page.id?[controller.pending.shape]:[];
  if(!page.shapes.length&&!pending.length&&!controller.active)return null;
  const point=(event:React.PointerEvent):[number,number]=>{const box=svg.current!.getBoundingClientRect();
    return [Math.min(width,Math.max(0,(event.clientX-box.left)/box.width*width)),Math.min(height,Math.max(0,(event.clientY-box.top)/box.height*height))];};
  const finish=(event:React.PointerEvent)=>{
    if(!drag||drag.pointer!==event.pointerId)return;
    const end=point(event);setDrag(null);
    const geometry:[number,number,number,number]=[drag.start[0],drag.start[1],end[0],end[1]];
    const line=LINE_KINDS.includes(controller.kind);
    if(line?Math.hypot(end[0]-drag.start[0],end[1]-drag.start[1])<4:Math.abs(end[0]-drag.start[0])<4||Math.abs(end[1]-drag.start[1])<4){
      controller.setError('Şekli çizmek için sayfada sürükleyin.');return;}
    void controller.submit(page.id,geometry);
  };
  return <>
    {current&&controller.active&&(reason||controller.error)&&<p className="ink-notice shape-notice" role={controller.error?'alert':undefined}>{controller.error||reason}</p>}
    <svg ref={svg} className={`shape-overlay ${drawing?'drawing':''}`} viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none"
      aria-label={drawing?`${pageIndex+1}. sayfa şekil alanı`:undefined}
      onPointerDown={event=>{if(!drawing||event.button!==0)return;event.preventDefault();controller.setError('');
        svg.current?.setPointerCapture(event.pointerId);const start=point(event);setDrag({pointer:event.pointerId,start,end:start});}}
      onPointerMove={event=>{if(drag&&drag.pointer===event.pointerId)setDrag({...drag,end:point(event)});}}
      onPointerUp={finish} onPointerCancel={()=>setDrag(null)}>
      {page.shapes.map(shape=><ShapeSvg key={shape.id} shape={shape}/>)}
      {pending.map(shape=><ShapeSvg key="pending" shape={shape} className="shape-pending"/>)}
      {drag&&<ShapeSvg className="shape-preview" shape={{kind:controller.kind,geometry:[...drag.start,...drag.end],colorARGB:controller.style.colorARGB,width:controller.style.width,
        fillARGB:controller.style.fill&&!LINE_KINDS.includes(controller.kind)?controller.style.fillARGB:null}}/>}
    </svg>
  </>;
}

const KIND_ICONS:Record<ShapeKind,ReactElement>={rectangle:<Square size={18}/>,ellipse:<Circle size={18}/>,triangle:<Triangle size={18}/>,
  rightTriangle:<svg width={18} height={18} viewBox="0 0 18 18" aria-hidden="true"><path d="M3 3 L15 15 L3 15 Z" fill="none" stroke="currentColor" strokeWidth={1.8}/></svg>,
  diamond:<Diamond size={18}/>,line:<Minus size={18}/>,arrow:<MoveUpRight size={18}/>};

export function ShapePanel({controller}:{controller:ShapeController}){
  const {style,setStyle,kind}=controller;const line=LINE_KINDS.includes(kind);
  return <div className="pen-panel shape-panel"><h2>Şekil</h2>
    <div className="shape-kinds" role="radiogroup" aria-label="Şekil türü">{SHAPE_KINDS.map(item=><button key={item.kind} role="radio" aria-checked={kind===item.kind} className={kind===item.kind?'selected':''} title={item.label} aria-label={item.label} onClick={()=>controller.setKind(item.kind)}>{KIND_ICONS[item.kind]}</button>)}</div>
    <div className="palette-heading pen-heading"><Palette size={16}/><span>Çizgi rengi</span></div>
    <div className="color-palette pen-colors" role="group" aria-label="Şekil çizgi rengi">{shapeColors.map(value=>{const argb=argbOf(value);const selected=argb===style.colorARGB>>>0;return <button key={value} aria-label={`Çizgi rengi ${value.toUpperCase()}`} aria-pressed={selected} className={selected?'selected':''} style={{backgroundColor:value}} onClick={()=>setStyle({...style,colorARGB:argb})}/>;})}</div>
    <label className="pen-size"><span>Kalınlık <output>{Math.round(style.width*10)/10}</output></span><input type="range" aria-label="Şekil çizgi kalınlığı" min={MIN_SHAPE_WIDTH} max={MAX_SHAPE_WIDTH} step={0.5} value={style.width} onChange={event=>setStyle({...style,width:Number(event.target.value)})}/></label>
    <label className="shape-fill"><input type="checkbox" disabled={line} checked={style.fill&&!line} onChange={event=>setStyle({...style,fill:event.target.checked})}/><span>Dolgu</span></label>
    {style.fill&&!line&&<div className="color-palette pen-colors" role="group" aria-label="Şekil dolgu rengi">{shapeColors.map(value=>{const argb=argbOf(value,0x40);const selected=argb===style.fillARGB>>>0;return <button key={value} aria-label={`Dolgu rengi ${value.toUpperCase()}`} aria-pressed={selected} className={selected?'selected':''} style={{backgroundColor:value,opacity:.5}} onClick={()=>setStyle({...style,fillARGB:argb})}/>;})}</div>}
    <p className="pen-hint">Sayfada sürükleyerek çizin; her şekil ayrı kaydedilip Samsung Cloud'a gönderilir. Önizleme yaklaşık geometridir, Samsung çizicisiyle birebir değildir. Şekil eklenen notta kalem şimdilik kapanır.</p>
  </div>;
}
