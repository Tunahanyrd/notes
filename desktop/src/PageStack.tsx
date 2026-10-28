import {useEffect, useRef, useState, type ReactNode} from 'react';
import {api, ApiError, notePath, request} from './api';
import type {PreviewPage} from './usePagePreview';

const hasGeometry=(p:PreviewPage)=>typeof p.width==='number'&&typeof p.height==='number'&&p.width>0&&p.height>0;
type Manifest={noteId:string;revision:number|undefined;pages:PreviewPage[];phase:'loading'|'ready'|'unavailable'};

// The note's page list, fetched once per revision. Images are loaded per page
// by PageStack, so scrolling never refetches the manifest.
export function usePageManifest(noteId:string|null,revision:number|undefined,enabled:boolean){
  const [state,setState]=useState<Manifest>({noteId:'',revision:undefined,pages:[],phase:'unavailable'});
  const [current,setCurrent]=useState({noteId:'',index:0});
  useEffect(()=>{
    if(!noteId||!enabled){setState({noteId:'',revision:undefined,pages:[],phase:'unavailable'});return;}
    const controller=new AbortController();
    setState(previous=>previous.noteId===noteId&&previous.revision!==revision&&previous.phase==='ready'?previous:{noteId,revision,pages:[],phase:'loading'});
    void api<{pages:PreviewPage[];providerAvailable:boolean}>(notePath(noteId)+'/pages','GET',undefined,controller.signal).then(result=>{
      const pages=(Array.isArray(result.pages)?result.pages:[]).filter(p=>Number.isInteger(p.index)&&p.index>=0&&p.index<10000).map(p=>({...p,available:p.available===true&&typeof p.width==='number'&&typeof p.height==='number'&&p.width>0&&p.height>0}));
      if(controller.signal.aborted)return;
      // A page without an image but with known geometry is still shown (calm
      // placeholder, ink on a blank sheet).
      setState({noteId,revision,pages,phase:result.providerAvailable&&pages.some(p=>p.available||hasGeometry(p))?'ready':'unavailable'});
    }).catch(()=>{if(!controller.signal.aborted)setState({noteId,revision,pages:[],phase:'unavailable'});});
    return()=>controller.abort();
  },[noteId,revision,enabled]);
  const mine=state.noteId===noteId?state:{noteId:'',revision:undefined,pages:[] as PreviewPage[],phase:'unavailable' as const};
  return {pages:mine.pages,phase:mine.phase,pageIndex:current.noteId===noteId?current.index:(mine.pages.find(p=>p.available)??mine.pages.find(hasGeometry))?.index??0,
    setPageIndex:(index:number)=>setCurrent({noteId:noteId||'',index})};
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
type ImageState={url:string|null;phase:'loading'|'ready'|'waiting'|'final'};
const sleep=(ms:number,signal:AbortSignal)=>new Promise<void>((resolve,reject)=>{
  if(signal.aborted){reject(new DOMException('Aborted','AbortError'));return;}
  const onAbort=()=>{clearTimeout(timer);reject(new DOMException('Aborted','AbortError'));};
  const timer=setTimeout(()=>{signal.removeEventListener('abort',onAbort);resolve();},ms);
  signal.addEventListener('abort',onAbort,{once:true});
});
function usePageImage(noteId:string,revision:number|undefined,index:number,wanted:boolean,visible:boolean){
  // The previous image stays visible until the next revision's image is ready.
  const [state,setState]=useState<ImageState>({url:null,phase:'loading'});const shown=useRef<string|null>(null);
  // Bumped to try again after the retries ran out: the page came back into
  // view, the window regained focus or the connection returned.
  const [epoch,setEpoch]=useState(0);const exhausted=useRef(false);
  useEffect(()=>()=>{if(shown.current)URL.revokeObjectURL(shown.current);},[]);
  useEffect(()=>{
    const again=()=>{if(exhausted.current){exhausted.current=false;setEpoch(value=>value+1);}};
    window.addEventListener('online',again);window.addEventListener('focus',again);
    return()=>{window.removeEventListener('online',again);window.removeEventListener('focus',again);};
  },[]);
  useEffect(()=>{if(visible&&exhausted.current){exhausted.current=false;setEpoch(value=>value+1);}},[visible]);
  useEffect(()=>{
    if(!wanted)return;
    const controller=new AbortController();exhausted.current=false;
    setState(previous=>({url:previous.url,phase:previous.url?'ready':'loading'}));
    const run=async()=>{
      let retries=0,polls=0;
      for(;;){
        let response:Response|null=null,final=false;
        try{response=await request(notePath(noteId)+`/preview?page=${index}`,{signal:controller.signal});}
        catch(error){
          if(error instanceof DOMException&&error.name==='AbortError')throw error;
          // 404: this page has no image source at all. Everything else may pass.
          final=error instanceof ApiError&&(error.status===404||error.status===400);
        }
        if(response?.status===202&&polls<MAX_PENDING_POLLS){polls++;await sleep(250,controller.signal);continue;}
        if(response&&response.status===200&&response.headers.get('content-type')?.split(';')[0].trim()==='image/png'){
          const blob=await response.blob();if(controller.signal.aborted)return;
          if(blob.size){
            const previous=shown.current;shown.current=URL.createObjectURL(blob);setState({url:shown.current,phase:'ready'});if(previous)URL.revokeObjectURL(previous);return;
          }
        }
        if(final){setState(previous=>previous.url?previous:{url:null,phase:'final'});return;}
        if(retries>=MAX_RETRIES){exhausted.current=true;setState(previous=>previous.url?previous:{url:null,phase:'waiting'});return;}
        setState(previous=>previous.url?previous:{url:null,phase:'waiting'});
        await sleep(BACKOFF_MS[Math.min(retries,BACKOFF_MS.length-1)],controller.signal);retries++;polls=0;
      }
    };
    void run().catch(()=>{});
    return()=>controller.abort();
  },[noteId,revision,index,wanted,epoch]);
  return state;
}

export type PageSize={width:number;height:number};
function PageItem({noteId,revision,page,isCurrent,register,render,renderUnavailable,size}:{noteId:string;revision:number|undefined;page:PreviewPage;isCurrent:boolean;register:(index:number,element:HTMLElement|null)=>void;render:(page:PreviewPage,url:string|null,isCurrent:boolean)=>ReactNode;renderUnavailable?:(page:PreviewPage,isCurrent:boolean)=>ReactNode;size?:PageSize}){
  const box=useRef<HTMLDivElement>(null);const [near,setNear]=useState(false);const [visible,setVisible]=useState(false);
  useEffect(()=>{
    const element=box.current;if(!element)return;register(page.index,element);
    const nearby=new IntersectionObserver(entries=>{const inside=entries.some(entry=>entry.isIntersecting);if(inside)setNear(true);setVisible(inside);},{rootMargin:'150% 0px'});
    nearby.observe(element);
    return()=>{nearby.disconnect();register(page.index,null);};
  },[page.index]);
  const {url,phase}=usePageImage(noteId,revision,page.index,near&&page.available,visible);
  // Page geometry from the manifest, else from the caller (ink projection).
  const width=page.width&&page.height?page.width:size?.width,height=page.width&&page.height?page.height:size?.height;
  const ratio=width&&height?`${width} / ${height}`:'1 / 1.414';
  const text=page.previewNotice||(!page.available||phase==='final'?PAGE_UNRENDERABLE:PAGE_PENDING);
  return <div ref={box} className={`page-sheet ${isCurrent?'current':''}`} style={{aspectRatio:ratio,backgroundColor:page.darkMode?'#000':'#fff'}} data-page-index={page.index} data-preview-phase={url?'ready':page.available?phase:'unavailable'}>
    {!url&&<div className="page-placeholder" role="status">{text}</div>}
    {!url&&!page.available&&renderUnavailable?.(page,isCurrent)}
    {(url||near)&&render(page,url,isCurrent)}
  </div>;
}

// All pages of a note stacked vertically like Samsung Notes. The current page
// is the one crossing a reading line 40% down the scroll area; `jump` scrolls.
export function PageStack({noteId,revision,pages,current,onCurrent,jump,render,renderUnavailable,pageSize}:{noteId:string;revision:number|undefined;pages:PreviewPage[];current:number;onCurrent:(index:number)=>void;jump:{index:number;seq:number}|null;render:(page:PreviewPage,url:string|null,isCurrent:boolean)=>ReactNode;renderUnavailable?:(page:PreviewPage,isCurrent:boolean)=>ReactNode;pageSize?:(index:number)=>PageSize|undefined}){
  const stack=useRef<HTMLDivElement>(null);const elements=useRef(new Map<number,HTMLElement>());
  const latest=useRef({current,onCurrent});latest.current={current,onCurrent};
  const register=(index:number,element:HTMLElement|null)=>{if(element)elements.current.set(index,element);else elements.current.delete(index);};
  useEffect(()=>{
    const container=stack.current?.closest('.canvas-scroll') as HTMLElement|null;if(!container)return;
    let frame=0;
    const measure=()=>{frame=0;const box=container.getBoundingClientRect();const line=box.top+box.height*.4;let best:number|null=null,distance=Infinity;
      for(const [index,element] of elements.current){const rect=element.getBoundingClientRect();const gap=rect.top>line?rect.top-line:rect.bottom<line?line-rect.bottom:0;if(gap<distance||(gap===distance&&best!==null&&index<best)){best=index;distance=gap;}}
      if(best!==null&&best!==latest.current.current)latest.current.onCurrent(best);};
    const schedule=()=>{if(!frame)frame=requestAnimationFrame(measure);};
    container.addEventListener('scroll',schedule,{passive:true});const resize=new ResizeObserver(schedule);resize.observe(container);schedule();
    return()=>{container.removeEventListener('scroll',schedule);resize.disconnect();if(frame)cancelAnimationFrame(frame);};
  },[pages.length]);
  useEffect(()=>{if(jump)elements.current.get(jump.index)?.scrollIntoView({block:'start'});},[jump?.seq]);
  return <div ref={stack} className="page-stack">{pages.map(page=><PageItem key={page.index} noteId={noteId} revision={revision} page={page} isCurrent={page.index===current} register={register} render={render} renderUnavailable={renderUnavailable} size={pageSize?.(page.index)}/>)}</div>;
}
