import {useEffect,useState} from 'react';
import {Circle,CircleCheck,FileText} from 'lucide-react';
import {WinIcon} from './WindowsChrome';
import {useThumbnail} from './useThumbnail';
import type {Note} from './types';
interface Props {note:Note;text:string;dirty:boolean;enabled:boolean;refresh:number;selectionMode:boolean;selected:boolean;band?:string}
export function NoteThumbnail({note,text,dirty,enabled,refresh,selectionMode,selected,band}:Props){
  const thumbnail=useThumbnail(note.id,note.revision,enabled&&!dirty,refresh);const [broken,setBroken]=useState(false);
  useEffect(()=>setBroken(false),[thumbnail.url]);
  const image=thumbnail.phase==='ready'&&thumbnail.url&&!broken;
  return <div className="note-thumbnail" ref={thumbnail.element}>
    {image?<img className="thumbnail-image" src={thumbnail.url!} alt="" draggable={false} decoding="async" onError={()=>setBroken(true)}/>:text?<div className="thumbnail-text">{text}</div>:<div className="thumbnail-empty"><FileText size={24}/><span>{thumbnail.phase==='pending'?'Önizleme hazırlanıyor':'Önizleme yok'}</span></div>}
    {dirty?<span className="thumbnail-text-only">Kaydedilmedi</span>:image?!thumbnail.complete&&<span className="thumbnail-text-only">Kısmi önizleme</span>:text&&!note.editable&&!band&&<span className="thumbnail-text-only">Yalnızca metin</span>}
    {selectionMode&&<span className="selection-check">{selected?<CircleCheck size={21}/>:<Circle size={21}/>}</span>}
    {band&&<span className="thumbnail-band">{band}</span>}
    {note.favorite&&<WinIcon name="ac_ic_favorite_on" size={15}/>}
  </div>;
}
