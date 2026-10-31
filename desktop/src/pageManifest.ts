import {api, notePath} from './api';
import type {PreviewPage} from './usePagePreview';

type PageManifest={pages:PreviewPage[];providerAvailable:boolean;pending?:boolean};

// `pending` describes inventory preparation, not an empty/unsupported note.
// The caller owns the timeout and cancellation for its current note/revision.
export async function loadPageManifest(noteId:string,signal:AbortSignal):Promise<PageManifest>{
  for(;;){
    const result=await api<PageManifest>(notePath(noteId)+'/pages','GET',undefined,signal);
    if(result.pending!==true)return result;
    await new Promise<void>((resolve,reject)=>{
      if(signal.aborted){reject(new DOMException('Aborted','AbortError'));return;}
      const onAbort=()=>{clearTimeout(timer);reject(new DOMException('Aborted','AbortError'));};
      const timer=setTimeout(()=>{signal.removeEventListener('abort',onAbort);resolve();},250);
      signal.addEventListener('abort',onAbort,{once:true});
    });
  }
}
