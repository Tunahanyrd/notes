import {useEffect, useMemo, useRef, useState, type DragEvent, type MouseEvent} from 'react';
import {notePath, request} from './api';
import type {Note} from './types';
import './page-sorter.css';

// One page as GET /api/notes/:id/page-ops returns it. `index` is the page's
// position in the saved revision; preview images are requested by it.
export type SorterPage={id:string;index:number;width:number;height:number;hasStrokes:boolean;deletable:boolean;reason:string|null};

type Props={
  note:Note;
  pages:SorterPage[];
  busy?:boolean;
  blockedReason?:string|null;     // e.g. pending sync or an unsaved ink draft
  canAdd?:boolean;
  onReorder:(ids:string[])=>void;
  onDelete:(ids:string[])=>void;
  onAdd:(afterIndex:number)=>void;
  close:()=>void;
};

function useSorterImage(noteId:string,revision:number,index:number,wanted:boolean){
  const [url,setUrl]=useState<string|null>(null);const [failed,setFailed]=useState(false);const shown=useRef<string|null>(null);
  useEffect(()=>()=>{if(shown.current)URL.revokeObjectURL(shown.current);},[]);
  useEffect(()=>{
    if(!wanted)return;
    const controller=new AbortController();setFailed(false);
    const run=async()=>{
      for(let attempt=0;attempt<240;attempt++){
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

function Thumb({note,page,position,selected,dragging,dropBefore,onPointer,onDragStart,onDragOver,onDrop,onDragEnd}:{note:Note;page:SorterPage;position:number;selected:boolean;dragging:boolean;dropBefore:boolean;
  onPointer:(event:MouseEvent)=>void;onDragStart:()=>void;onDragOver:(event:DragEvent)=>void;onDrop:(event:DragEvent)=>void;onDragEnd:()=>void}){
  const box=useRef<HTMLDivElement>(null);const [near,setNear]=useState(false);
  useEffect(()=>{const element=box.current;if(!element)return;const seen=new IntersectionObserver(entries=>{if(entries.some(e=>e.isIntersecting))setNear(true);},{rootMargin:'200px'});seen.observe(element);return()=>seen.disconnect();},[]);
  const {url,failed}=useSorterImage(note.id,note.revision,page.index,near);
  const ratio=page.width>0&&page.height>0?`${page.width} / ${page.height}`:'1 / 1.414';
  return <div ref={box} role="option" aria-selected={selected} tabIndex={-1} draggable
    className={`sorter-page${selected?' selected':''}${dragging?' dragging':''}${dropBefore?' drop-before':''}`}
    onClick={onPointer} onDragStart={event=>{event.dataTransfer.effectAllowed='move';event.dataTransfer.setData('text/plain',String(position));onDragStart();}}
    onDragOver={onDragOver} onDrop={onDrop} onDragEnd={onDragEnd}>
    <div className="sorter-sheet" style={{aspectRatio:ratio}}>
      {url?<img src={url} alt="" draggable={false}/>:<span className="sorter-placeholder">{failed?'Görüntü yok':''}</span>}
    </div>
    <span className="sorter-number">{position+1}</span>
  </div>;
}

// Page organiser modelled on the Windows app's page grid: drag to reorder,
// select to delete, add a blank page after the selection. Changes are queued
// Cloud edits; the caller refreshes `pages` after each verified/queued change.
export function PageSorter({note,pages,busy=false,blockedReason=null,canAdd=true,onReorder,onDelete,onAdd,close}:Props){
  const [order,setOrder]=useState(()=>pages.map(p=>p.id));
  const [selected,setSelected]=useState<string[]>([]);
  const [anchor,setAnchor]=useState<string|null>(null);
  const [drag,setDrag]=useState<{from:number;over:number|null}|null>(null);
  const [confirmDelete,setConfirmDelete]=useState(false);
  const grid=useRef<HTMLDivElement>(null);
  const key=pages.map(p=>p.id).join(',');
  useEffect(()=>{setOrder(pages.map(p=>p.id));setSelected(s=>s.filter(id=>pages.some(p=>p.id===id)));setConfirmDelete(false);},[key,note.revision]);
  const byId=useMemo(()=>new Map(pages.map(p=>[p.id,p])),[pages]);
  const ordered=order.map(id=>byId.get(id)).filter((p):p is SorterPage=>!!p);
  const reordered=order.join(',')!==key;
  const locked=busy||!!blockedReason;
  const deletable=selected.length>0&&selected.length<pages.length&&selected.every(id=>byId.get(id)?.deletable);
  const deleteReason=!selected.length?null:selected.length>=pages.length?'Notta en az bir sayfa kalmalı.':selected.map(id=>byId.get(id)).find(p=>p&&!p.deletable)?.reason||null;
  useEffect(()=>{
    const onKey=(event:KeyboardEvent)=>{
      if(event.key==='Escape'){event.preventDefault();if(confirmDelete)setConfirmDelete(false);else close();return;}
      if((event.key==='Delete')&&deletable&&!locked&&!reordered){event.preventDefault();setConfirmDelete(true);return;}
      if(event.altKey&&(event.key==='ArrowLeft'||event.key==='ArrowRight')&&selected.length===1&&!locked){
        event.preventDefault();const from=order.indexOf(selected[0]);const to=from+(event.key==='ArrowLeft'?-1:1);
        if(to<0||to>=order.length)return;const next=[...order];next.splice(from,1);next.splice(to,0,selected[0]);setOrder(next);
      }
      if((event.ctrlKey||event.metaKey)&&event.key.toLowerCase()==='a'){event.preventDefault();setSelected([...order]);}
    };
    document.addEventListener('keydown',onKey);return()=>document.removeEventListener('keydown',onKey);
  },[confirmDelete,deletable,locked,reordered,selected,order,close]);
  useEffect(()=>{grid.current?.focus();},[]);
  const pointer=(id:string)=>(event:MouseEvent)=>{
    setConfirmDelete(false);
    if(event.shiftKey&&anchor){const a=order.indexOf(anchor),b=order.indexOf(id);setSelected(order.slice(Math.min(a,b),Math.max(a,b)+1));return;}
    if(event.ctrlKey||event.metaKey){setSelected(s=>s.includes(id)?s.filter(x=>x!==id):[...s,id]);setAnchor(id);return;}
    setSelected([id]);setAnchor(id);
  };
  const drop=(target:number)=>(event:DragEvent)=>{
    event.preventDefault();if(!drag||locked){setDrag(null);return;}
    const ids=selected.includes(order[drag.from])?order.filter(id=>selected.includes(id)):[order[drag.from]];
    const rest=order.filter(id=>!ids.includes(id));
    const before=order.slice(0,target).filter(id=>!ids.includes(id)).length;
    rest.splice(before,0,...ids);setOrder(rest);setDrag(null);
  };
  const afterIndex=selected.length?Math.max(...selected.map(id=>order.indexOf(id))):order.length-1;
  return <div className="sorter-backdrop" onMouseDown={event=>{if(event.target===event.currentTarget)close();}}>
    <div className="sorter-dialog" role="dialog" aria-modal="true" aria-label="Sayfaları düzenle">
      <header className="sorter-header">
        <h2>Sayfaları düzenle</h2>
        <div className="sorter-actions">
          <button type="button" disabled={locked||reordered||!canAdd} title={reordered?'Önce yeni sırayı uygulayın.':undefined}
            onClick={()=>onAdd(afterIndex)}>Sayfa ekle</button>
          {confirmDelete
            ?<><span className="sorter-confirm">{selected.length} sayfa silinsin mi?</span>
              <button type="button" className="danger" disabled={locked} onClick={()=>{setConfirmDelete(false);onDelete(order.filter(id=>selected.includes(id)));}}>Sil</button>
              <button type="button" onClick={()=>setConfirmDelete(false)}>Vazgeç</button></>
            :<button type="button" disabled={locked||reordered||!deletable} title={deleteReason||undefined} onClick={()=>setConfirmDelete(true)}>Sil</button>}
          {reordered&&<>
            <button type="button" className="primary" disabled={locked} onClick={()=>onReorder(order)}>Sırayı uygula</button>
            <button type="button" onClick={()=>setOrder(pages.map(p=>p.id))}>Geri al</button></>}
          <button type="button" className="sorter-close" aria-label="Kapat" onClick={close}>✕</button>
        </div>
      </header>
      {(blockedReason||deleteReason&&selected.length)&&<p className="sorter-status" role="status">{blockedReason||deleteReason}</p>}
      <div ref={grid} className="sorter-grid" role="listbox" aria-multiselectable="true" aria-label="Sayfalar" tabIndex={0}
        onDragOver={event=>{if(drag)event.preventDefault();}} onDrop={drop(order.length)}>
        {ordered.map((page,position)=><Thumb key={page.id} note={note} page={page} position={position}
          selected={selected.includes(page.id)} dragging={!!drag&&drag.from===position} dropBefore={!!drag&&drag.over===position&&drag.from!==position}
          onPointer={pointer(page.id)} onDragStart={()=>setDrag({from:position,over:null})}
          onDragOver={event=>{if(!drag||locked)return;event.preventDefault();event.stopPropagation();if(drag.over!==position)setDrag({...drag,over:position});}}
          onDrop={event=>{event.stopPropagation();drop(position)(event);}} onDragEnd={()=>setDrag(null)}/>)}
      </div>
      <footer className="sorter-footer">Sürükleyerek sırala · Ctrl/Shift ile çoklu seçim · Alt+←/→ taşı · Delete sil · Esc kapat</footer>
    </div>
  </div>;
}
