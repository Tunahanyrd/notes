// Live pen preview: strokes are drawn by the same sdocx stroke renderer +
// resvg rasterizer as saved pages, compiled to WebAssembly
// (crates/notes-ink-live; tools/build_ink_wasm.sh writes public/ink-live.wasm)
// and run in a Web Worker. When the module is missing or refuses a pen,
// callers keep the simple SVG polyline preview; nothing is shown to the user.
import {useEffect,useState} from 'react';
import type {InkPoint} from './InkEditor';

// Template pen exactly as the page renderer reads it (GET …/ink-pens).
export interface InkPenTemplate {width:number;height:number;rendering:Record<string,unknown>}
export interface LivePenSpec {rendering?:Record<string,unknown>;colorARGB:number;size:number;pageWidth:number;pageHeight:number}
// Device-pixel raster of one stroke (straight-alpha RGBA) at `scale` device
// pixels per page unit.
export interface LiveRaster {x:number;y:number;width:number;height:number;scale:number;image:ImageData}
type Reply={id:number;ok:boolean;x?:number;y?:number;width?:number;height?:number;data?:ArrayBuffer};
const MAX_RASTERS=256,MAX_RASTER_BYTES=96*1024*1024;

// FNV-1a over the stored samples; arrays are hashed once (WeakMap).
const hashes=new WeakMap<readonly InkPoint[],{length:number;hash:string}>();
function pointsHash(points:readonly InkPoint[]){
  const known=hashes.get(points);if(known&&known.length===points.length)return known.hash;
  let hash=0x811c9dc5;const mix=(value:number)=>{const text=String(value);for(let i=0;i<text.length;i++){hash^=text.charCodeAt(i);hash=Math.imul(hash,0x01000193);}hash^=44;hash=Math.imul(hash,0x01000193);};
  for(const point of points){mix(point.x);mix(point.y);mix(point.pressure);mix(point.timeMs);}
  const value=`${points.length}.${(hash>>>0).toString(36)}`;hashes.set(points,{length:points.length,hash:value});return value;
}

export class LiveRenderer {
  private next=1;
  private waiting=new Map<number,{scale:number;done:(raster:LiveRaster|null)=>void}>();
  private bytes=0;
  private specs=new Map<string,number>();
  private rasters=new Map<string,LiveRaster|null>();
  private inflight=new Map<string,Promise<LiveRaster|null>>();
  constructor(private worker:Worker){
    worker.onmessage=(event:MessageEvent<Reply>)=>{
      const reply=event.data,request=this.waiting.get(reply.id);if(!request)return;this.waiting.delete(reply.id);
      request.done(reply.ok&&reply.data&&reply.width&&reply.height?{x:reply.x!,y:reply.y!,width:reply.width,height:reply.height,scale:request.scale,image:new ImageData(new Uint8ClampedArray(reply.data),reply.width,reply.height)}:null);
    };
  }
  // Short stable id of a pen spec (part of raster keys).
  specKey(spec:LivePenSpec){const text=JSON.stringify(spec);let id=this.specs.get(text);if(id===undefined){id=this.specs.size+1;this.specs.set(text,id);}return `${id}`;}
  key(spec:LivePenSpec,points:readonly InkPoint[],scale:number){return `${this.specKey(spec)}|${scale.toFixed(4)}|${pointsHash(points)}`;}
  // One render, not cached (the stroke under the pen).
  render(spec:LivePenSpec,points:readonly InkPoint[],scale:number):Promise<LiveRaster|null>{
    if(points.length<2)return Promise.resolve(null);
    const values=new Float64Array(points.length*4);points.forEach((point,i)=>{values[i*4]=point.x;values[i*4+1]=point.y;values[i*4+2]=point.pressure;values[i*4+3]=point.timeMs;});
    const id=this.next++;
    return new Promise(resolve=>{this.waiting.set(id,{scale,done:resolve});this.worker.postMessage({type:'render',id,spec:JSON.stringify(spec),points:values,scale},[values.buffer]);});
  }
  // Cached raster of a finished stroke: undefined while it is being made.
  cached(key:string){return this.rasters.get(key);}
  store(key:string,raster:LiveRaster|null){
    const size=(value:LiveRaster|null|undefined)=>value?value.image.data.byteLength:0;
    this.bytes-=size(this.rasters.get(key));this.rasters.delete(key);
    while(this.rasters.size&&(this.rasters.size>=MAX_RASTERS||this.bytes+size(raster)>MAX_RASTER_BYTES)){const [oldest,value]=this.rasters.entries().next().value!;this.bytes-=size(value);this.rasters.delete(oldest);}
    this.rasters.set(key,raster);this.bytes+=size(raster);
  }
  ensure(spec:LivePenSpec,points:readonly InkPoint[],scale:number):Promise<LiveRaster|null>{
    const key=this.key(spec,points,scale);if(this.rasters.has(key))return Promise.resolve(this.rasters.get(key)!);
    let pending=this.inflight.get(key);
    if(!pending){pending=this.render(spec,points,scale).then(raster=>{this.inflight.delete(key);this.store(key,raster);return raster;});this.inflight.set(key,pending);}
    return pending;
  }
}

let renderer:LiveRenderer|null=null;let loading:Promise<LiveRenderer|null>|null=null;
// Starts the worker once; resolves null when the module is missing or cannot start.
export function loadInkLive():Promise<LiveRenderer|null>{
  if(renderer)return Promise.resolve(renderer);
  if(loading)return loading;
  loading=new Promise<LiveRenderer|null>(resolve=>{
    let worker:Worker;
    try{
      if(typeof Worker!=='function'||typeof WebAssembly!=='object'){resolve(null);return;}
      worker=new Worker(new URL('./inkLiveWorker.ts',import.meta.url),{type:'module'});
    }catch{resolve(null);return;}
    const timer=setTimeout(()=>{worker.terminate();resolve(null);},15000);
    worker.onerror=()=>{clearTimeout(timer);worker.terminate();resolve(null);};
    worker.onmessage=(event:MessageEvent<{type?:string}>)=>{
      clearTimeout(timer);
      if(event.data?.type==='ready'){renderer=new LiveRenderer(worker);resolve(renderer);}else{worker.terminate();resolve(null);}
    };
    worker.postMessage({type:'init',url:new URL('ink-live.wasm',document.baseURI).href});
  });
  return loading;
}
export function useInkLive(enabled:boolean){
  const [value,setValue]=useState<LiveRenderer|null>(renderer);
  useEffect(()=>{if(!enabled||value)return;let live=true;void loadInkLive().then(result=>{if(live&&result)setValue(result);});return()=>{live=false;};},[enabled,value]);
  return enabled?value:null;
}
