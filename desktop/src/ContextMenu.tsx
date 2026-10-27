import {useEffect,useLayoutEffect,useRef,useState,type ReactNode} from 'react';
import {createPortal} from 'react-dom';
export interface ContextTarget {kind:'folder'|'note'|'trashFolder';id:string;ids:string[];x:number;y:number;trigger:HTMLElement}
export interface ContextAction {label:string;disabled?:boolean;description?:string;icon?:ReactNode;run:()=>void}
export function ContextMenu({target,actions,onClose}:{target:ContextTarget;actions:ContextAction[];onClose:()=>void}){
  const menu=useRef<HTMLDivElement>(null);const close=useRef(onClose);close.current=onClose;
  const [position,setPosition]=useState({x:target.x,y:target.y});
  useLayoutEffect(()=>{const element=menu.current;if(!element)return;setPosition({x:Math.max(8,Math.min(target.x,window.innerWidth-element.offsetWidth-8)),y:Math.max(8,Math.min(target.y,window.innerHeight-element.offsetHeight-8))});element.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus({preventScroll:true});},[target,actions.length]);
  useEffect(()=>{
    const outside=(event:PointerEvent)=>{if(!menu.current?.contains(event.target as Node))close.current();};
    const scroll=(event:Event)=>{if(!menu.current?.contains(event.target as Node))close.current();};
    const resize=()=>close.current();
    const keys=(event:KeyboardEvent)=>{
      if(event.key==='Escape'){event.preventDefault();close.current();target.trigger.isConnected&&target.trigger.focus({preventScroll:true});return;}
      if(event.key==='Tab'){close.current();return;}
      if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;
      event.preventDefault();const items=[...menu.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')||[]];if(!items.length)return;
      const current=items.indexOf(document.activeElement as HTMLButtonElement),next=event.key==='Home'?0:event.key==='End'?items.length-1:(current+(event.key==='ArrowDown'?1:-1)+items.length)%items.length;items[next].focus();
    };
    document.addEventListener('pointerdown',outside,true);document.addEventListener('keydown',keys);window.addEventListener('scroll',scroll,true);window.addEventListener('resize',resize);
    return()=>{document.removeEventListener('pointerdown',outside,true);document.removeEventListener('keydown',keys);window.removeEventListener('scroll',scroll,true);window.removeEventListener('resize',resize);};
  },[target]);
  return createPortal(<div ref={menu} className="context-menu" role="menu" aria-label={target.kind==='note'?'Not menüsü':'Klasör menüsü'} style={{left:position.x,top:position.y}} onContextMenu={event=>event.preventDefault()}>{actions.map(action=><button role="menuitem" key={action.label} disabled={action.disabled} title={action.description} onClick={()=>{target.trigger.isConnected&&target.trigger.focus({preventScroll:true});close.current();action.run();}}><span>{action.label}</span></button>)}</div>,document.body);
}
