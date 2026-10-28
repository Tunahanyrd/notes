import {useEffect, useId, useRef, useState, type KeyboardEvent} from 'react';
import {FolderMinus} from 'lucide-react';
import {WinIcon} from './WindowsChrome';
import './folder-picker.css';

// One row of a folder list. id '' is the library root ("Klasörsüz" for notes,
// "Klasörler" as a folder's parent); icon 'none' draws the no-folder glyph.
export interface FolderOption {id:string;label:string;depth:number;count?:number|string;reason?:string;current?:boolean;icon?:'none'}

// A light Windows 11 / Samsung Notes listbox replacing the native <select>
// (WebKitGTK draws that as a dark GTK popup). Selection follows the keyboard
// focus, disabled rows explain themselves and are skipped, Enter chooses.
export function FolderPicker({options,value,onChange,onChoose,label,autoFocus}:{options:FolderOption[];value:string|null;onChange:(id:string)=>void;onChoose:(id:string)=>void;label:string;autoFocus?:boolean}) {
  const base=useId().replace(/:/g,'');
  const listRef=useRef<HTMLDivElement>(null);
  const initial=()=>{const index=options.findIndex(option=>value!==null?option.id===value:option.current);return index>=0?index:options.findIndex(option=>!option.reason);};
  const [active,setActive]=useState(initial);
  const typed=useRef({text:'',at:0});
  useEffect(()=>{if(value===null)return;const index=options.findIndex(option=>option.id===value);if(index>=0)setActive(index);},[value,options]);
  useEffect(()=>{const row=listRef.current?.querySelector<HTMLElement>(`[data-index="${active}"]`);row?.scrollIntoView({block:'nearest'});},[active]);
  const enabled=(index:number)=>index>=0&&index<options.length&&!options[index].reason;
  const go=(index:number)=>{if(!enabled(index))return;setActive(index);onChange(options[index].id);};
  const step=(from:number,direction:1|-1)=>{for(let index=from+direction;index>=0&&index<options.length;index+=direction)if(enabled(index))return index;return -1;};
  const key=(event:KeyboardEvent<HTMLDivElement>)=>{
    const last=options.length;
    let next=-1;
    if(event.key==='ArrowDown')next=step(active,1);
    else if(event.key==='ArrowUp')next=step(active,-1);
    else if(event.key==='Home')next=step(-1,1);
    else if(event.key==='End')next=step(last,-1);
    else if(event.key==='PageDown'||event.key==='PageUp'){const direction=event.key==='PageDown'?1:-1;let index=active;for(let n=0;n<8;n++){const candidate=step(index,direction);if(candidate<0)break;index=candidate;}next=index===active?-1:index;}
    else if(event.key==='Enter'||event.key===' '){event.preventDefault();if(enabled(active)){onChange(options[active].id);if(event.key==='Enter')onChoose(options[active].id);}return;}
    else if(event.key.length===1&&!event.ctrlKey&&!event.metaKey&&!event.altKey){
      // Type-ahead: repeated first letter cycles, a quick word jumps to its prefix.
      const now=Date.now(),letter=event.key.toLocaleLowerCase('tr-TR');
      typed.current=now-typed.current.at<700&&typed.current.text!==letter?{text:typed.current.text+letter,at:now}:{text:letter,at:now};
      const prefix=typed.current.text,startAfter=prefix.length>1?active-1:active;
      for(let n=1;n<=last;n++){const index=(startAfter+n+last)%last;if(enabled(index)&&options[index].label.toLocaleLowerCase('tr-TR').startsWith(prefix)){next=index;break;}}
      if(next<0)return;
    }else return;
    event.preventDefault();
    if(next>=0)go(next);
  };
  const activeId=active>=0&&active<options.length?`${base}-${active}`:undefined;
  return <div ref={listRef} className="folder-picker" role="listbox" tabIndex={0} aria-label={label} aria-activedescendant={activeId} data-autofocus={autoFocus?'':undefined} onKeyDown={key}>
    {options.map((option,index)=>{
      const selected=value===option.id,disabled=!!option.reason;
      return <div key={option.id||'(root)'} id={`${base}-${index}`} data-index={index} data-folder-id={option.id} role="option" aria-selected={selected} aria-disabled={disabled||undefined} aria-current={option.current?'location':undefined}
        className={`folder-option ${selected?'selected':''} ${index===active?'active':''} ${disabled?'disabled':''}`} title={option.reason||option.label} style={{paddingInlineStart:12+option.depth*20}}
        onPointerDown={event=>{if(event.button===0)event.preventDefault();listRef.current?.focus();}}
        onClick={()=>go(index)} onDoubleClick={()=>{if(enabled(index)){go(index);onChoose(option.id);}}}>
        {option.icon==='none'?<FolderMinus size={17} className="folder-option-root" aria-hidden="true"/>:<WinIcon name="ic_drawer_folder" size={18}/>}
        <span className="folder-option-label">{option.label}</span>
        {option.current&&<span className="folder-option-badge">Şu anki konum</span>}
        {option.count!==undefined&&option.count!==''&&<small>{option.count}</small>}
      </div>;
    })}
  </div>;
}
