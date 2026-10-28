import {useEffect, useState} from 'react';
import {api, notePath, request} from './api';
export type PreviewPage={index:number;width:number|null;height:number|null;available:boolean;darkMode?:boolean;previewKind?:'pdf-background'|'pdf-with-notes'|'ink-reconstructed'|'page-reconstructed'|'blank'|string;complete?:boolean;previewNotice?:string|null};
type PreviewState={noteId:string;pages:PreviewPage[];phase:'loading'|'ready'|'unavailable';url:string|null;activeIndex:number};
const empty:PreviewState={noteId:'',pages:[],phase:'unavailable',url:null,activeIndex:0};
export function usePagePreview(noteId:string|null,revision:number|undefined,enabled:boolean) {
  const [state,setState]=useState<PreviewState>(empty);
  const [selection,setSelection]=useState({noteId:'',index:0});
  const pageIndex=selection.noteId===noteId?selection.index:0;
  useEffect(()=>{
    if(!noteId||!enabled){setState(empty);return;}
    const controller=new AbortController();let objectUrl:string|null=null;let disposed=false;
    let delayTimer:ReturnType<typeof setTimeout>|undefined;
    const timeout=setTimeout(()=>controller.abort(),120_000);
    const update=(patch:Partial<PreviewState>)=>{if(!controller.signal.aborted)setState(previous=>({...previous,...patch}));};
    setState({noteId,pages:[],phase:'loading',url:null,activeIndex:pageIndex});
    const run=async()=>{
      const result=await api<{pages:PreviewPage[];providerAvailable:boolean}>(notePath(noteId)+'/pages','GET',undefined,controller.signal);
      if(!result.providerAvailable||!Array.isArray(result.pages)||!result.pages.length||result.pages.length>2000)throw Error('preview_unavailable');
      const pages=result.pages.filter(p=>Number.isInteger(p.index)&&p.index>=0&&p.index<10000).map(p=>({...p,available:p.available===true&&typeof p.width==='number'&&typeof p.height==='number'&&Number.isFinite(p.width)&&Number.isFinite(p.height)&&p.width>0&&p.height>0}));
      if(!pages.length)throw Error('preview_unavailable');
      const chosen=pages.find(p=>p.index===pageIndex&&p.available)||pages.find(p=>p.available);
      update({pages,activeIndex:chosen?.index??0});
      if(!chosen)throw Error('preview_unavailable');
      for(;;){
        const response=await request(notePath(noteId)+`/preview?page=${chosen.index}`,{signal:controller.signal});
        if(response.status===202){
          await new Promise<void>((resolve,reject)=>{
            const onAbort=()=>{clearTimeout(delayTimer);reject(new DOMException('Aborted','AbortError'));};
            if(controller.signal.aborted){onAbort();return;}
            controller.signal.addEventListener('abort',onAbort,{once:true});
            delayTimer=setTimeout(()=>{controller.signal.removeEventListener('abort',onAbort);resolve();},250);
          });
          continue;
        }
        if(response.headers.get('content-type')?.split(';')[0].trim()!=='image/png')throw Error('preview_unavailable');
        const blob=await response.blob();
        if(controller.signal.aborted)return;
        if(!blob.size||blob.size>32*1024*1024)throw Error('preview_unavailable');
        objectUrl=URL.createObjectURL(blob);update({phase:'ready',url:objectUrl});return;
      }
    };
    void run().catch(()=>{if(!disposed)setState(previous=>previous.noteId===noteId?{...previous,phase:'unavailable',url:null}:previous);}).finally(()=>clearTimeout(timeout));
    return()=>{disposed=true;controller.abort();clearTimeout(timeout);clearTimeout(delayTimer);if(objectUrl)URL.revokeObjectURL(objectUrl);};
  },[noteId,revision,enabled,pageIndex]);
  return {...(state.noteId===noteId?state:empty),pageIndex:state.noteId===noteId?state.activeIndex:0,setPageIndex:(index:number)=>setSelection({noteId:noteId||'',index})};
}
