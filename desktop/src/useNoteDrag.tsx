import {useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent} from 'react';
import {FileText} from 'lucide-react';

// Press-and-hold drag of note cards onto sidebar drop targets
// (elements carrying data-drop-target: a folder id, or '' for the root).
// A short click still opens the note and a right click still opens the
// context menu: the drag starts only after HOLD_MS with the pointer
// (mouse, pen or touch) barely moving. The drop calls the caller's move
// action; this hook never writes anything itself.
const HOLD_MS=380,SLOP=8,EXPAND_MS=650;
type Target=string|null;
export interface NoteDragOptions {
  canDrag:(noteId:string)=>boolean;
  idsFor:(noteId:string)=>string[];
  titleFor:(ids:string[])=>string;
  noteReason:(ids:string[])=>string;
  reason:(ids:string[],target:Target)=>string;
  targetLabel:(target:Target)=>string;
  onDrop:(ids:string[],target:Target)=>void;
  onExpand?:(folderId:string)=>void;
}
interface DragState {ids:string[];title:string;thumb:string|null;hover:Target|undefined;valid:boolean;baseReason:string}
interface Press {pointerId:number;x:number;y:number;noteId:string;element:HTMLElement;timer:number;dragging:boolean}

export function useNoteDrag(options:NoteDragOptions) {
  const optionsRef=useRef(options);optionsRef.current=options;
  const [drag,setDrag]=useState<DragState|null>(null);
  const dragRef=useRef<DragState|null>(null);
  const press=useRef<Press|null>(null);
  const position=useRef({x:0,y:0});
  const ghost=useRef<HTMLDivElement|null>(null);
  const suppressUntil=useRef(0);
  const suppressNote=useRef<string|null>(null);
  const expand=useRef<{id:string;timer:number}|null>(null);
  const listeners=useRef<(()=>void)|null>(null);
  const update=(next:DragState|null)=>{dragRef.current=next;setDrag(next);};

  const place=useCallback(()=>{
    const node=ghost.current;if(!node)return;
    const {x,y}=position.current,width=node.offsetWidth,height=node.offsetHeight;
    node.style.left=`${Math.max(4,Math.min(x+14,window.innerWidth-width-4))}px`;
    node.style.top=`${Math.max(4,Math.min(y+12,window.innerHeight-height-4))}px`;
  },[]);
  const ghostRef=useCallback((node:HTMLDivElement|null)=>{ghost.current=node;place();},[place]);

  const clearExpand=()=>{if(expand.current){window.clearTimeout(expand.current.timer);expand.current=null;}};
  const finish=()=>{
    const current=press.current;press.current=null;
    if(current){window.clearTimeout(current.timer);if(current.dragging){suppressUntil.current=performance.now()+600;suppressNote.current=current.noteId;try{if(current.element.hasPointerCapture(current.pointerId))current.element.releasePointerCapture(current.pointerId);}catch{/* pointer already gone */}}}
    clearExpand();listeners.current?.();listeners.current=null;
    document.documentElement.classList.remove('note-dragging');
    if(dragRef.current)update(null);
  };
  const hitTest=(x:number,y:number)=>{
    const state=dragRef.current;if(!state)return;
    const element=document.elementFromPoint(x,y)?.closest<HTMLElement>('[data-drop-target]');
    const hover:Target|undefined=element?(element.dataset.dropTarget||null):undefined;
    if(expand.current&&expand.current.id!==hover)clearExpand();
    if(hover&&element?.dataset.dropExpand!==undefined&&!expand.current){const id=hover;expand.current={id,timer:window.setTimeout(()=>{expand.current=null;optionsRef.current.onExpand?.(id);},EXPAND_MS)};}
    if(hover===state.hover)return;
    const valid=hover!==undefined&&!optionsRef.current.reason(state.ids,hover);
    update({...state,hover,valid});
  };
  const start=()=>{
    const current=press.current;if(!current)return;
    const o=optionsRef.current,ids=o.idsFor(current.noteId);
    if(!ids.length){finish();return;}
    current.dragging=true;
    try{current.element.setPointerCapture(current.pointerId);}catch{/* pointer released meanwhile */}
    window.getSelection()?.removeAllRanges();
    document.documentElement.classList.add('note-dragging');
    const thumb=ids.length===1?current.element.querySelector<HTMLImageElement>('img.thumbnail-image')?.getAttribute('src')??null:null;
    // A note-level block (pending sync, unsaved ink…) makes every target invalid.
    const baseReason=o.noteReason(ids);
    update({ids,title:o.titleFor(ids),thumb,hover:undefined,valid:false,baseReason});
    hitTest(position.current.x,position.current.y);
  };
  const onPointerDown=(noteId:string)=>(event:ReactPointerEvent<HTMLElement>)=>{
    if(event.button!==0||!event.isPrimary||press.current||!optionsRef.current.canDrag(noteId))return;
    const element=event.currentTarget;
    suppressUntil.current=0;suppressNote.current=null;// a new press is a new click intent
    position.current={x:event.clientX,y:event.clientY};
    press.current={pointerId:event.pointerId,x:event.clientX,y:event.clientY,noteId,element,timer:window.setTimeout(start,HOLD_MS),dragging:false};
    const move=(e:PointerEvent)=>{
      const current=press.current;if(!current||e.pointerId!==current.pointerId)return;
      position.current={x:e.clientX,y:e.clientY};
      if(!current.dragging){if(Math.hypot(e.clientX-current.x,e.clientY-current.y)>SLOP)finish();return;}
      e.preventDefault();place();hitTest(e.clientX,e.clientY);
    };
    const up=(e:PointerEvent)=>{
      const current=press.current;if(!current||e.pointerId!==current.pointerId)return;
      const state=dragRef.current;
      if(current.dragging&&state){position.current={x:e.clientX,y:e.clientY};hitTest(e.clientX,e.clientY);}
      const final=dragRef.current;
      finish();
      if(current.dragging&&final&&final.hover!==undefined&&final.valid)optionsRef.current.onDrop(final.ids,final.hover);
    };
    const cancel=(e:PointerEvent)=>{if(press.current&&e.pointerId===press.current.pointerId)finish();};
    const key=(e:KeyboardEvent)=>{if(e.key==='Escape'&&press.current){e.preventDefault();e.stopPropagation();finish();}};
    const block=(e:Event)=>{if(press.current?.dragging||performance.now()<suppressUntil.current){e.preventDefault();e.stopPropagation();}};
    const touch=(e:TouchEvent)=>{if(press.current?.dragging)e.preventDefault();};
    const blur=()=>finish();
    window.addEventListener('pointermove',move,{capture:true,passive:false});
    window.addEventListener('pointerup',up,true);
    window.addEventListener('pointercancel',cancel,true);
    window.addEventListener('keydown',key,true);
    window.addEventListener('contextmenu',block,true);
    window.addEventListener('selectstart',block,true);
    window.addEventListener('touchmove',touch,{capture:true,passive:false});
    window.addEventListener('blur',blur);
    listeners.current=()=>{
      window.removeEventListener('pointermove',move,true);window.removeEventListener('pointerup',up,true);window.removeEventListener('pointercancel',cancel,true);
      window.removeEventListener('keydown',key,true);window.removeEventListener('touchmove',touch,true);window.removeEventListener('blur',blur);
      // The context menu a touch long-press raises arrives just after release: keep blocking briefly.
      window.setTimeout(()=>{window.removeEventListener('contextmenu',block,true);window.removeEventListener('selectstart',block,true);},600);
    };
  };
  useEffect(()=>()=>finish(),[]);// eslint-disable-line react-hooks/exhaustive-deps

  const bind=(noteId:string)=>({onPointerDown:onPointerDown(noteId),onDragStart:(event:{preventDefault:()=>void})=>event.preventDefault()});
  // The click a drag's release produces on its own card (drop or cancel) must not open the note.
  const consumeClick=(noteId:string)=>{const hit=suppressNote.current===noteId&&performance.now()<suppressUntil.current;if(hit){suppressUntil.current=0;suppressNote.current=null;}return hit;};
  const isSource=(noteId:string)=>!!drag?.ids.includes(noteId);
  const isTarget=(target:Target)=>!!drag&&drag.valid&&drag.hover===target;
  const caption=!drag?'':drag.hover!==undefined?(drag.valid?options.targetLabel(drag.hover):options.reason(drag.ids,drag.hover)):drag.baseReason||'Bir klasörün üzerine bırakın';
  const blocked=!!drag&&(drag.hover!==undefined?!drag.valid:!!drag.baseReason);
  const ghostElement=drag?<div ref={ghostRef} className="note-drag-ghost" role="status" aria-live="polite" data-testid="note-drag-ghost">
    <span className={`note-drag-thumb ${drag.ids.length>1?'note-drag-stack':''}`} style={drag.thumb?{backgroundImage:`url("${drag.thumb.replace(/"/g,'%22')}")`}:undefined}>{!drag.thumb&&<FileText size={18}/>}</span>
    <span className="note-drag-text"><strong>{drag.title}</strong><small className={blocked?'blocked':''}>{caption}</small></span>
  </div>:null;
  return {bind,consumeClick,isSource,isTarget,ghost:ghostElement,active:!!drag};
}
