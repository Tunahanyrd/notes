import {createContext,useEffect, useLayoutEffect, useRef, useState, type ReactNode} from 'react';
import {api, ApiError, notePath, request} from './api';
import {loadPageManifest} from './pageManifest';
import type {PreviewPage} from './usePagePreview';

const hasGeometry=(p:PreviewPage)=>typeof p.width==='number'&&typeof p.height==='number'&&p.width>0&&p.height>0;
type Manifest={noteId:string;revision:number|undefined;pages:PreviewPage[];phase:'loading'|'ready'|'unavailable'|'failed'};
export const InkPageContext=createContext<{noteId:string;revision:number|undefined;page:PreviewPage;visible:boolean;textless:boolean;suppressTiles:boolean;onTextLayerReady?:(index:number)=>void;onTextLayerFailed?:(index:number)=>void}|null>(null);

// The tile service uses device pixels per page unit, not zoom percentage.
// Refuse requests above the supported range rather than choosing a blurry
// smaller bucket.
export function inkTileBucket(scale:number):number|null{
  if(!Number.isFinite(scale)||scale<=0)return null;
  return [1,1.5,2,3,4].find(bucket=>bucket>=scale)??null;
}
// Snap in viewport coordinates so a fractional page origin is accounted for;
// rounding an offset alone does not place its edge on the device pixel grid.
export function snapInkOffset(pageOrigin:number,offset:number,dpr:number):number|null{
  if(!Number.isFinite(pageOrigin)||!Number.isFinite(offset)||!Number.isFinite(dpr)||dpr<=0)return null;
  return Math.round((pageOrigin+offset)*dpr)/dpr-pageOrigin;
}
type InkTileBounds={columnStart:number;columnEnd:number;rowStart:number;rowEnd:number};
export function visibleInkTiles(page:{left:number;top:number;width:number;height:number},viewport:{left:number;top:number;right:number;bottom:number},pageSize:PageSize,bucket:number,margin=1):InkTileBounds|null{
  const values=[page.left,page.top,page.width,page.height,viewport.left,viewport.top,viewport.right,viewport.bottom,pageSize.width,pageSize.height,bucket,margin];
  if(values.some(value=>!Number.isFinite(value))||page.width<=0||page.height<=0||pageSize.width<=0||pageSize.height<=0||![1,1.5,2,3,4].includes(bucket)||!Number.isInteger(margin)||margin<0||margin>4)return null;
  const left=Math.max(page.left,viewport.left),top=Math.max(page.top,viewport.top),right=Math.min(page.left+page.width,viewport.right),bottom=Math.min(page.top+page.height,viewport.bottom);
  if(left>=right||top>=bottom)return null;
  const columns=Math.ceil(pageSize.width*bucket/512),rows=Math.ceil(pageSize.height*bucket/512);
  const x=(value:number)=>(value-page.left)/page.width*pageSize.width*bucket/512;
  const y=(value:number)=>(value-page.top)/page.height*pageSize.height*bucket/512;
  return {columnStart:Math.max(0,Math.floor(x(left))-margin),columnEnd:Math.min(columns-1,Math.ceil(x(right))-1+margin),rowStart:Math.max(0,Math.floor(y(top))-margin),rowEnd:Math.min(rows-1,Math.ceil(y(bottom))-1+margin)};
}

// The note's page list, fetched once per revision. Images are loaded per page
// by PageStack, so scrolling never refetches the manifest.
export function usePageManifest(noteId:string|null,revision:number|undefined,enabled:boolean){
  const [state,setState]=useState<Manifest>({noteId:'',revision:undefined,pages:[],phase:'unavailable'});
  const [current,setCurrent]=useState({noteId:'',index:0});
  const [retryEpoch,setRetryEpoch]=useState(0);
  const retryRequested=useRef<string|null>(null);
  useEffect(()=>{
    if(!noteId||!enabled){retryRequested.current=null;setState({noteId:'',revision:undefined,pages:[],phase:'unavailable'});return;}
    if(retryRequested.current&&retryRequested.current!==noteId)retryRequested.current=null;
    const controller=new AbortController();let disposed=false,timedOut=false;
    const timeout=setTimeout(()=>{timedOut=true;controller.abort();},120_000);
    setState(previous=>previous.noteId===noteId&&previous.revision!==revision&&previous.phase==='ready'?previous:{noteId,revision,pages:[],phase:'loading'});
    const load=async()=>{
      if(retryRequested.current===noteId){
        await api(notePath(noteId)+'/pages','POST',{},controller.signal);
        if(controller.signal.aborted)return null;
        retryRequested.current=null;
      }
      return loadPageManifest(noteId,controller.signal);
    };
    void load().then(result=>{
      if(!result)return;
      const pages=(Array.isArray(result.pages)?result.pages:[]).filter(p=>Number.isInteger(p.index)&&p.index>=0&&p.index<10000).map(p=>({...p,available:p.available===true&&typeof p.width==='number'&&typeof p.height==='number'&&p.width>0&&p.height>0}));
      if(controller.signal.aborted)return;
      // A page without an image but with known geometry is still shown (calm
      // placeholder, ink on a blank sheet).
      setState({noteId,revision,pages,phase:result.providerAvailable&&pages.some(p=>p.available||hasGeometry(p))?'ready':'unavailable'});
    }).catch(error=>{if(!disposed&&(timedOut||!controller.signal.aborted)){retryRequested.current=null;setState({noteId,revision,pages:[],phase:error instanceof ApiError&&error.code==='preview_inventory_failed'?'failed':'unavailable'});}}).finally(()=>clearTimeout(timeout));
    return()=>{disposed=true;clearTimeout(timeout);controller.abort();};
  },[noteId,revision,enabled,retryEpoch]);
  const mine=state.noteId===noteId?state:{noteId:'',revision:undefined,pages:[] as PreviewPage[],phase:'unavailable' as const};
  return {pages:mine.pages,phase:mine.phase,pageIndex:current.noteId===noteId?current.index:(mine.pages.find(p=>p.available)??mine.pages.find(hasGeometry))?.index??0,
    setPageIndex:(index:number)=>setCurrent({noteId:noteId||'',index}),
    retry:()=>{if(noteId){retryRequested.current=noteId;setRetryEpoch(value=>value+1);}}};
}

// Calm placeholder copy: a page image that is not ready yet is never an error.
export const PAGE_PENDING='Bu sayfanın görüntüsü henüz hazır değil.';
export const PAGE_UNRENDERABLE='Bu sayfanın görüntüsü bu cihazda oluşturulamıyor.';
// Retries for transient answers (busy, offline, a failed job with a short
// retry window, network errors). 202 means "still preparing" and is polled
// at once; the server itself waits several seconds per request.
const BACKOFF_MS=[1000,2000,5000,10000,20000,30000];
const MAX_RETRIES=12;
const MAX_PENDING_POLLS=480;
type ImageState={url:string|null;phase:'loading'|'ready'|'waiting'|'final';key?:string};
const sleep=(ms:number,signal:AbortSignal)=>new Promise<void>((resolve,reject)=>{
  if(signal.aborted){reject(new DOMException('Aborted','AbortError'));return;}
  const onAbort=()=>{clearTimeout(timer);reject(new DOMException('Aborted','AbortError'));};
  const timer=setTimeout(()=>{signal.removeEventListener('abort',onAbort);resolve();},ms);
  signal.addEventListener('abort',onAbort,{once:true});
});
function usePageImage(noteId:string,revision:number|undefined,index:number,wanted:boolean,visible:boolean,retain:boolean,variant:'normal'|'notext'|'fulltext',refreshKey:string){
  // The previous image stays visible until the next revision's image is ready.
  const [state,setState]=useState<ImageState>({url:null,phase:'loading'});const shown=useRef<string|null>(null);const retired=useRef<string[]>([]);const fetched=useRef<string|null>(null);
  // Bumped to try again after the retries ran out: the page came back into
  // view, the window regained focus or the connection returned.
  const [epoch,setEpoch]=useState(0);const exhausted=useRef(false);
  // A page on screen never gives up: after the backoff it keeps asking every
  // 30 s (the engine answers from its cache once the image exists).
  const onScreen=useRef(visible);onScreen.current=visible;
  useEffect(()=>{for(const url of retired.current.splice(0))URL.revokeObjectURL(url);},[state.url]);
  useEffect(()=>()=>{if(shown.current)URL.revokeObjectURL(shown.current);for(const url of retired.current.splice(0))URL.revokeObjectURL(url);},[]);
  useEffect(()=>{
    const again=()=>{if(exhausted.current){exhausted.current=false;setEpoch(value=>value+1);}};
    window.addEventListener('online',again);window.addEventListener('focus',again);
    return()=>{window.removeEventListener('online',again);window.removeEventListener('focus',again);};
  },[]);
  useEffect(()=>{if(visible&&exhausted.current){exhausted.current=false;setEpoch(value=>value+1);}},[visible]);
  useEffect(()=>{
    if(!wanted){
      if(!retain){if(shown.current){URL.revokeObjectURL(shown.current);shown.current=null;}fetched.current=null;setState({url:null,phase:'loading'});}
      return;
    }
    const key=`${noteId}:${revision}:${index}:${variant}:${refreshKey}`;if(shown.current&&fetched.current===key)return;
    const controller=new AbortController();exhausted.current=false;
    setState(previous=>({...previous,phase:previous.url?'ready':'loading'}));
    const run=async()=>{
      let retries=0,polls=0;
      for(;;){
        let response:Response|null=null,final=false;
        try{response=await request(notePath(noteId)+`/preview?page=${index}`+(variant!=='normal'?`&variant=${variant}${revision===undefined?'':`&revision=${revision}`}`:''),{signal:controller.signal});}
        catch(error){
          if(error instanceof DOMException&&error.name==='AbortError')throw error;
          // 404: this page has no image source at all. Everything else may pass.
          final=error instanceof ApiError&&(error.status===404||error.status===400);
        }
        if(response?.status===202&&(polls<MAX_PENDING_POLLS||onScreen.current)){polls++;await sleep(250,controller.signal);continue;}
        if(response&&response.status===200&&response.headers.get('content-type')?.split(';')[0].trim()==='image/png'){
          const blob=await response.blob();if(controller.signal.aborted)return;
          if(blob.size){
            const next=URL.createObjectURL(blob);const image=new Image();image.src=next;
            try{await image.decode();}catch{
              if(variant!=='normal'){URL.revokeObjectURL(next);setState(previous=>({...previous,phase:'final'}));return;}
              // Preserve the established fallback for old normal-preview fixtures.
            }
            if(controller.signal.aborted){URL.revokeObjectURL(next);return;}
            const previous=shown.current;shown.current=next;fetched.current=key;if(previous)retired.current.push(previous);
            setState({url:next,phase:'ready',key});return;
          }
        }
        if(final){setState(previous=>variant==='normal'&&previous.url?previous:{...previous,phase:'final'});return;}
        if(retries>=MAX_RETRIES&&!onScreen.current){exhausted.current=true;setState(previous=>previous.url?previous:{url:null,phase:'waiting'});return;}
        setState(previous=>previous.url?previous:{url:null,phase:'waiting'});
        await sleep(BACKOFF_MS[Math.min(retries,BACKOFF_MS.length-1)],controller.signal);retries++;polls=0;
      }
    };
    void run().catch(()=>{});
    return()=>controller.abort();
  },[noteId,revision,index,wanted,retain,epoch,variant,refreshKey]);
  return state;
}

export type PageSize={width:number;height:number};
function PageItem({noteId,revision,page,isCurrent,register,render,renderUnavailable,size,variant,refreshKey,onVariantReady,onVariantFailed,onTextLayerReady,onTextLayerFailed,onPageDoubleClick}:{noteId:string;revision:number|undefined;page:PreviewPage;isCurrent:boolean;register:(index:number,element:HTMLElement|null)=>void;render:(page:PreviewPage,url:string|null,isCurrent:boolean)=>ReactNode;renderUnavailable?:(page:PreviewPage,isCurrent:boolean)=>ReactNode;size?:PageSize;variant:'normal'|'notext'|'fulltext';refreshKey:string;onVariantReady?:(index:number,variant:'normal'|'notext'|'fulltext')=>void;onVariantFailed?:(index:number,variant:'normal'|'notext'|'fulltext')=>void;onTextLayerReady?:(index:number)=>void;onTextLayerFailed?:(index:number)=>void;onPageDoubleClick?:(page:PreviewPage,event:React.MouseEvent)=>void}){
  const box=useRef<HTMLDivElement>(null);const [near,setNear]=useState(false);const [visible,setVisible]=useState(false);
  useEffect(()=>{
    const element=box.current;if(!element)return;register(page.index,element);
    let evict:ReturnType<typeof setTimeout>|undefined;
    const nearby=new IntersectionObserver(entries=>{const inside=entries.some(entry=>entry.isIntersecting);clearTimeout(evict);setVisible(inside);
      // Resizing and zoom anchoring can briefly move a visible page outside
      // the observer margin. Keep its bitmap during that transient movement.
      if(inside)setNear(true);else evict=setTimeout(()=>setNear(false),800);
    },{rootMargin:'150% 0px'});
    nearby.observe(element);
    return()=>{clearTimeout(evict);nearby.disconnect();register(page.index,null);};
  },[page.index]);
  const {url,phase,key}=usePageImage(noteId,revision,page.index,visible&&page.available,visible,near,variant,refreshKey);
  const expected=`${noteId}:${revision}:${page.index}:${variant}:${refreshKey}`;
  useEffect(()=>{if(url&&key===expected)onVariantReady?.(page.index,variant);},[url,key,expected,variant,page.index,onVariantReady]);
  useEffect(()=>{if(variant!=='normal'&&phase==='final'&&key!==expected)onVariantFailed?.(page.index,variant);},[phase,key,expected,variant,page.index,onVariantFailed]);
  // Page geometry from the manifest, else from the caller (ink projection).
  const width=page.width&&page.height?page.width:size?.width,height=page.width&&page.height?page.height:size?.height;
  const ratio=width&&height?`${width} / ${height}`:'1 / 1.414';
  return <div ref={box} className={`page-sheet ${isCurrent?'current':''}`} style={{aspectRatio:ratio,backgroundColor:page.darkMode?'#000':'#fff'}} data-page-dark={String(page.darkMode===true)} data-page-index={page.index} data-preview-phase={url?'ready':page.available?phase:'unavailable'} onDoubleClick={event=>onPageDoubleClick?.(page,event)}>
    {!url&&<div className="page-placeholder" aria-hidden="true"/>}
    {!url&&!page.available&&renderUnavailable?.(page,isCurrent)}
    {(url||near)&&<InkPageContext.Provider value={{noteId,revision,page,visible,textless:variant==='notext',suppressTiles:variant==='fulltext',onTextLayerReady,onTextLayerFailed}}>{render(page,url,isCurrent)}</InkPageContext.Provider>}
  </div>;
}

// All pages of a note stacked vertically like Samsung Notes. The current page
// is the one crossing a reading line 40% down the scroll area; `jump` scrolls.
export function PageStack({noteId,revision,pages,current,onCurrent,jump,render,renderUnavailable,pageSize,zoom,variant='normal',refreshKey='',onVariantReady,onVariantFailed,onTextLayerReady,onTextLayerFailed,onPageDoubleClick,overlay}:{noteId:string;revision:number|undefined;pages:PreviewPage[];current:number;onCurrent:(index:number)=>void;jump:{index:number;seq:number}|null;render:(page:PreviewPage,url:string|null,isCurrent:boolean)=>ReactNode;renderUnavailable?:(page:PreviewPage,isCurrent:boolean)=>ReactNode;pageSize?:(index:number)=>PageSize|undefined;zoom?:number;variant?:'normal'|'notext'|'fulltext';refreshKey?:string;onVariantReady?:(index:number,variant:'normal'|'notext'|'fulltext')=>void;onVariantFailed?:(index:number,variant:'normal'|'notext'|'fulltext')=>void;onTextLayerReady?:(index:number)=>void;onTextLayerFailed?:(index:number)=>void;onPageDoubleClick?:(page:PreviewPage,event:React.MouseEvent)=>void;overlay?:ReactNode}){
  const stack=useRef<HTMLDivElement>(null);const elements=useRef(new Map<number,HTMLElement>());
  const latest=useRef({current,onCurrent});latest.current={current,onCurrent};
  // The point of the page on the reading line (40 % down), kept in place when
  // the zoom changes the page sizes; otherwise another page slides under it.
  const anchor=useRef<{index:number;fraction:number;x:number}|null>(null);
  const capture=()=>{
    const container=stack.current?.closest('.canvas-scroll') as HTMLElement|null;if(!container)return;
    const box=container.getBoundingClientRect();const line=box.top+box.height*.4;
    for(const [index,element] of elements.current){const rect=element.getBoundingClientRect();
      if(rect.top<=line&&rect.bottom>=line&&rect.height>0){anchor.current={index,fraction:(line-rect.top)/rect.height,x:container.scrollWidth>container.clientWidth?(container.scrollLeft+container.clientWidth/2)/container.scrollWidth:.5};return;}}
  };
  useLayoutEffect(()=>{
    const container=stack.current?.closest('.canvas-scroll') as HTMLElement|null;const kept=anchor.current;if(!container||!kept)return;
    const element=elements.current.get(kept.index);if(!element)return;
    const box=container.getBoundingClientRect(),rect=element.getBoundingClientRect();
    container.scrollTop+=rect.top+rect.height*kept.fraction-(box.top+box.height*.4);
    container.scrollLeft=kept.x*container.scrollWidth-container.clientWidth/2;
  },[zoom]);
  const register=(index:number,element:HTMLElement|null)=>{if(element)elements.current.set(index,element);else elements.current.delete(index);};
  useEffect(()=>{
    const container=stack.current?.closest('.canvas-scroll') as HTMLElement|null;if(!container)return;
    let frame=0;
    const measure=()=>{frame=0;capture();const box=container.getBoundingClientRect();const line=box.top+box.height*.4;let best:number|null=null,distance=Infinity;
      for(const [index,element] of elements.current){const rect=element.getBoundingClientRect();const gap=rect.top>line?rect.top-line:rect.bottom<line?line-rect.bottom:0;if(gap<distance||(gap===distance&&best!==null&&index<best)){best=index;distance=gap;}}
      if(best!==null&&best!==latest.current.current)latest.current.onCurrent(best);};
    const schedule=()=>{if(!frame)frame=requestAnimationFrame(measure);};
    // Page sizes change without a scroll event (zoom, images arriving): the
    // stack is observed too, so the page number follows.
    container.addEventListener('scroll',schedule,{passive:true});const resize=new ResizeObserver(schedule);resize.observe(container);if(stack.current)resize.observe(stack.current);schedule();
    return()=>{container.removeEventListener('scroll',schedule);resize.disconnect();if(frame)cancelAnimationFrame(frame);};
  },[pages.length]);
  useEffect(()=>{if(jump)elements.current.get(jump.index)?.scrollIntoView({block:'start'});},[jump?.seq]);
  return <div ref={stack} className="page-stack">{pages.map(page=><PageItem key={page.index} noteId={noteId} revision={revision} page={page} isCurrent={page.index===current} register={register} render={render} renderUnavailable={renderUnavailable} size={pageSize?.(page.index)} variant={variant} refreshKey={refreshKey} onVariantReady={onVariantReady} onVariantFailed={onVariantFailed} onTextLayerReady={onTextLayerReady} onTextLayerFailed={onTextLayerFailed} onPageDoubleClick={onPageDoubleClick}/>)}{overlay}</div>;
}
