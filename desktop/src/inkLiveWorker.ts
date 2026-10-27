// Web Worker hosting the live pen renderer (crates/notes-ink-live as
// WebAssembly) so rasterizing a stroke never blocks pointer input.
// Messages: {type:'init',url} → {type:'ready'}|{type:'failed'};
// {type:'render',id,spec,points(Float64Array of x,y,pressure,timeMs),scale}
// → {id,ok:true,x,y,width,height,data(ArrayBuffer RGBA)}|{id,ok:false}.
interface Exports {
  memory:WebAssembly.Memory;
  ink_alloc(len:number):number;
  ink_free(ptr:number,len:number):void;
  ink_pen_new(ptr:number,len:number):number;
  ink_pen_drop(id:number):void;
  ink_render(id:number,ptr:number,count:number,scale:number):number;
  ink_out_ptr():number;
  ink_out_len():number;
  ink_out_rect():number;
}
type WorkerRequest={type:'init';url:string}|{type:'render';id:number;spec:string;points:Float64Array;scale:number};
const scope=self as unknown as {onmessage:((event:MessageEvent<WorkerRequest>)=>void)|null;postMessage(message:unknown,transfer?:Transferable[]):void};
let exports:Exports|null=null;
const pens=new Map<string,number>();
const MAX_PENS=64;
function pen(spec:string):number{
  const e=exports!;const known=pens.get(spec);if(known!==undefined)return known;
  const bytes=new TextEncoder().encode(spec);const ptr=e.ink_alloc(bytes.length);
  new Uint8Array(e.memory.buffer,ptr,bytes.length).set(bytes);const id=e.ink_pen_new(ptr,bytes.length);e.ink_free(ptr,bytes.length);
  if(pens.size>=MAX_PENS){const [oldSpec,oldId]=pens.entries().next().value!;pens.delete(oldSpec);if(oldId)e.ink_pen_drop(oldId);}
  pens.set(spec,id);return id;
}
scope.onmessage=async event=>{
  const message=event.data;
  if(message.type==='init'){
    try{
      const response=await fetch(message.url);if(!response.ok)throw Error('missing');
      const {instance}=await WebAssembly.instantiate(await response.arrayBuffer(),{});
      const candidate=instance.exports as unknown as Exports;
      if(typeof candidate.ink_render!=='function'||!(candidate.memory instanceof WebAssembly.Memory))throw Error('invalid');
      exports=candidate;scope.postMessage({type:'ready'});
    }catch{scope.postMessage({type:'failed'});}
    return;
  }
  const e=exports;if(!e){scope.postMessage({id:message.id,ok:false});return;}
  try{
    const id=pen(message.spec);const count=message.points.length/4;
    if(!id||count<2){scope.postMessage({id:message.id,ok:false});return;}
    const len=message.points.length*8;const ptr=e.ink_alloc(len);
    new Float64Array(e.memory.buffer,ptr,message.points.length).set(message.points);
    const ok=e.ink_render(id,ptr,count,message.scale);e.ink_free(ptr,len);
    const [x,y,width,height]=new Int32Array(e.memory.buffer,e.ink_out_rect(),4);
    if(!ok||!width||!height){scope.postMessage({id:message.id,ok:false});return;}
    const data=new Uint8Array(e.ink_out_len());data.set(new Uint8Array(e.memory.buffer,e.ink_out_ptr(),data.length));
    scope.postMessage({id:message.id,ok:true,x,y,width,height,data:data.buffer},[data.buffer]);
  }catch{scope.postMessage({id:message.id,ok:false});}
};
export {};
