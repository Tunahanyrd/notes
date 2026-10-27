import {useEffect, useRef, useState, type ReactNode} from 'react';
import {api, notePath, request} from './api';
import type {PreviewPage} from './usePagePreview';

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
      setState({noteId,revision,pages,phase:result.providerAvailable&&pages.some(p=>p.available)?'ready':'unavailable'});
    }).catch(()=>{if(!controller.signal.aborted)setState({noteId,revision,pages:[],phase:'unavailable'});});
    return()=>controller.abort();
  },[noteId,revision,enabled]);
  const mine=state.noteId===noteId?state:{noteId:'',revision:undefined,pages:[] as PreviewPage[],phase:'unavailable' as const};
  return {pages:mine.pages,phase:mine.phase,pageIndex:current.noteId===noteId?current.index:mine.pages.find(p=>p.available)?.index??0,
    setPageIndex:(index:number)=>setCurrent({noteId:noteId||'',index})};
}

function usePageImage(noteId:string,revision:number|undefined,index:number,wanted:boolean){
  // The previous image stays visible until the next revision's image is ready.
  const [url,setUrl]=useState<string|null>(null);const [failed,setFailed]=useState(false);const shown=useRef<string|null>(null);
  useEffect(()=>()=>{if(shown.current)URL.revokeObjectURL(shown.current);},[]);
  useEffect(()=>{
    if(!wanted)return;
    const controller=new AbortController();setFailed(false);
    const run=async()=>{
      for(let attempt=0;attempt<480;attempt++){
        const response=await request(notePath(noteId)+`/preview?page=${index}`,{signal:controller.signal});
        if(response.status===202){await new Promise(resolve=>setTimeout(resolve,250));continue;}
        if(response.headers.get('content-type')?.split(';')[0].trim()!=='image/png')throw Error('preview_unavailable');
        const blob=await response.blob();if(controller.signal.aborted)return;
        const previous=shown.current;shown.current=URL.createObjectURL(blob);setUrl(shown.current);if(previous)URL.revokeObjectURL(previous);return;
      }
      throw Error('preview_timeout');
    };
    void run().catch(()=>{if(!controller.signal.aborted)setFailed(true);});
    return()=>controller.abort();
  },[noteId,revision,index,wanted]);
  return {url,failed};
}

function PageItem({noteId,revision,page,isCurrent,register,render,renderUnavailable}:{noteId:string;revision:number|undefined;page:PreviewPage;isCurrent:boolean;register:(index:number,element:HTMLElement|null)=>void;render:(page:PreviewPage,url:string,isCurrent:boolean)=>ReactNode;renderUnavailable?:(page:PreviewPage,isCurrent:boolean)=>ReactNode}){
  const box=useRef<HTMLDivElement>(null);const [near,setNear]=useState(false);
  useEffect(()=>{
    const element=box.current;if(!element)return;register(page.index,element);
    const nearby=new IntersectionObserver(entries=>{if(entries.some(entry=>entry.isIntersecting))setNear(true);},{rootMargin:'150% 0px'});
    nearby.observe(element);
    return()=>{nearby.disconnect();register(page.index,null);};
  },[page.index]);
  const {url,failed}=usePageImage(noteId,revision,page.index,near&&page.available);
  const ratio=page.available?`${page.width} / ${page.height}`:'1 / 1.414';
  return <div ref={box} className={`page-sheet ${isCurrent?'current':''}`} style={{aspectRatio:ratio,backgroundColor:page.darkMode?'#000':'#fff'}} data-page-index={page.index}>
    {url?render(page,url,isCurrent):(!page.available&&renderUnavailable?.(page,isCurrent))||<div className="page-placeholder">{!page.available?'Bu sayfanın görüntüsü henüz hazırlanamıyor.':failed?'Sayfa görüntüsü açılamadı.':null}</div>}
  </div>;
}

// All pages of a note stacked vertically like Samsung Notes. The current page
// is the one crossing a reading line 40% down the scroll area; `jump` scrolls.
export function PageStack({noteId,revision,pages,current,onCurrent,jump,render,renderUnavailable}:{noteId:string;revision:number|undefined;pages:PreviewPage[];current:number;onCurrent:(index:number)=>void;jump:{index:number;seq:number}|null;render:(page:PreviewPage,url:string,isCurrent:boolean)=>ReactNode;renderUnavailable?:(page:PreviewPage,isCurrent:boolean)=>ReactNode}){
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
  return <div ref={stack} className="page-stack">{pages.map(page=><PageItem key={page.index} noteId={noteId} revision={revision} page={page} isCurrent={page.index===current} register={register} render={render} renderUnavailable={renderUnavailable}/>)}</div>;
}
