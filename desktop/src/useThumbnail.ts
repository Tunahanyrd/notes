import {useEffect,useRef,useState} from 'react';
import {notePath,request} from './api';

type Pending={signal:AbortSignal;run:()=>Promise<void>;cancel:()=>void};
const queue:Pending[]=[];let active=0;
const aborted=()=>new DOMException('Aborted','AbortError');
function pump(){while(active<3&&queue.length){const job=queue.shift()!;job.signal.removeEventListener('abort',job.cancel);if(job.signal.aborted){job.cancel();continue;}active++;void job.run().finally(()=>{active--;pump();});}}
function scheduled<T>(run:()=>Promise<T>,signal:AbortSignal){return new Promise<T>((resolve,reject)=>{const job:Pending={signal,run:async()=>{try{resolve(await run());}catch(error){reject(error);}},cancel:()=>{const index=queue.indexOf(job);if(index>=0)queue.splice(index,1);reject(aborted());}};if(signal.aborted){reject(aborted());return;}signal.addEventListener('abort',job.cancel,{once:true});queue.push(job);pump();});}
const cache=new Map<string,{blob:Blob;complete:boolean}>();let cacheBytes=0;
function cached(key:string){const value=cache.get(key);if(value){cache.delete(key);cache.set(key,value);}return value;}
function remember(key:string,value:{blob:Blob;complete:boolean}){const old=cache.get(key);if(old)cacheBytes-=old.blob.size;cache.delete(key);cache.set(key,value);cacheBytes+=value.blob.size;while(cache.size>64||cacheBytes>8*1024*1024){const oldest=cache.keys().next().value!;cacheBytes-=cache.get(oldest)!.blob.size;cache.delete(oldest);}}
type Thumbnail={key:string;url:string|null;phase:'idle'|'pending'|'ready'|'unavailable';complete:boolean};
export function useThumbnail(id:string,revision:number,enabled:boolean,refresh:number){
  const element=useRef<HTMLDivElement>(null);const [visible,setVisible]=useState(false);
  const key=`${id}\u0000${revision}\u0000${refresh}`;
  const attempts=useRef({key,number:0,terminal:false});
  const [state,setState]=useState<Thumbnail>({key,url:null,phase:'idle',complete:true});
  useEffect(()=>{const target=element.current;if(!target)return;const observer=new IntersectionObserver(entries=>setVisible(entries.some(entry=>entry.isIntersecting)),{threshold:0.01});observer.observe(target);return()=>observer.disconnect();},[]);
  useEffect(()=>{
    if(attempts.current.key!==key)attempts.current={key,number:0,terminal:false};
    if(!enabled||!visible||!Number.isInteger(revision)||revision<=0){setState({key,url:null,phase:'idle',complete:true});return;}
    const controller=new AbortController();let url:string|null=null;let delay:ReturnType<typeof setTimeout>|undefined;let disposed=false;
    const update=(value:Partial<Thumbnail>)=>{if(!disposed&&!controller.signal.aborted)setState(previous=>({...previous,...value,key}));};
    const show=(value:{blob:Blob;complete:boolean})=>{if(disposed||controller.signal.aborted)return;url=URL.createObjectURL(value.blob);update({url,phase:'ready',complete:value.complete});};
    const hit=cached(key);if(hit){show(hit);return()=>{disposed=true;if(url)URL.revokeObjectURL(url);};}
    if(attempts.current.terminal){setState({key,url:null,phase:'unavailable',complete:true});return;}
    setState({key,url:null,phase:'pending',complete:true});
    let deadline:ReturnType<typeof setTimeout>|undefined;
    const pause=(milliseconds:number)=>new Promise<void>((resolve,reject)=>{const cancel=()=>{clearTimeout(delay);reject(aborted());};controller.signal.addEventListener('abort',cancel,{once:true});delay=setTimeout(()=>{controller.signal.removeEventListener('abort',cancel);resolve();},milliseconds);});
    const run=async()=>{
      deadline=setTimeout(()=>controller.abort(),90_000);
      while(attempts.current.number<30){
        const {response,blob}=await (async()=>{
          attempts.current.number++;
          const response=await request(notePath(id)+`/thumbnail?revision=${revision}`,{signal:controller.signal});
          if(response.status===202)return {response,blob:null};
          if(response.headers.get('content-type')?.split(';')[0].trim()!=='image/png')throw Error('thumbnail_unavailable');
          const size=Number(response.headers.get('content-length'));if(size>2*1024*1024)throw Error('thumbnail_too_large');
          const blob=await response.blob();if(!blob.size||blob.size>2*1024*1024)throw Error('thumbnail_unavailable');return {response,blob};
        })();
        if(response.status===202){const seconds=Number(response.headers.get('Retry-After'));await pause(Number.isFinite(seconds)&&seconds>0?Math.min(2000,Math.max(1000,seconds*1000)):1500);continue;}
        if(!blob)throw Error('thumbnail_unavailable');
        if(disposed||controller.signal.aborted)return;
        const value={blob,complete:response.headers.get('X-Notes-Preview-Complete')!=='false'};remember(key,value);show(value);return;
      }
      throw Error('thumbnail_timeout');
    };
    void scheduled(run,controller.signal).catch(()=>{if(!disposed){attempts.current.terminal=true;setState({key,url:null,phase:'unavailable',complete:true});}}).finally(()=>clearTimeout(deadline));
    return()=>{disposed=true;controller.abort();clearTimeout(deadline);clearTimeout(delay);if(url)URL.revokeObjectURL(url);};
  },[id,revision,key,visible,enabled]);
  const current=state.key===key?state:{key,url:null,phase:'idle' as const,complete:true};
  return {...current,element};
}
