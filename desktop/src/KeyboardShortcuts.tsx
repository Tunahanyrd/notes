import {useEffect,useState} from 'react';
import {Keyboard,RotateCcw,Save,X} from 'lucide-react';
import {DEFAULT_SHORTCUTS,FIXED_SHORTCUTS,SHORTCUT_ACTIONS,SHORTCUT_GROUPS,formatShortcut,modifierWords,shortcutFromEvent,validateBindings,validateShortcut,type ShortcutAction,type ShortcutBindings,type ShortcutsController} from './useShortcuts';
import './keyboard-shortcuts.css';

export function KeyboardShortcuts({controller}:{controller:ShortcutsController}) {
  const [draft,setDraft]=useState<ShortcutBindings>({...controller.bindings});
  const [capturing,setCapturing]=useState<ShortcutAction|null>(null);
  const [error,setError]=useState(''),[saved,setSaved]=useState(false);
  useEffect(()=>{setDraft({...controller.bindings});setError('');},[controller.bindings]);
  const invalid=validateBindings(draft);
  const changed=SHORTCUT_ACTIONS.some(a=>draft[a.id]!==controller.bindings[a.id]);
  const blocked=controller.loading||controller.saving||!controller.loaded;
  const apply=async()=>{if(await controller.save(draft)){setSaved(true);setCapturing(null);}};
  const assign=(action:ShortcutAction,combo:string)=>{
    const candidate={...draft,[action]:combo};
    const conflict=validateBindings(candidate);
    if(conflict){setError(conflict);setSaved(false);return;}
    setDraft(candidate);setError('');setSaved(false);
  };
  return <div className="keyboard-shortcuts">
    <p className="shortcut-intro"><Keyboard size={16} aria-hidden="true"/>Windows Samsung Notes kısayolları varsayılandır. Değiştirmek için bir kısayola tıklayıp yeni tuş birleşimine basın.</p>
    {SHORTCUT_GROUPS.map(group=><section className="shortcut-group" key={group.id} aria-label={group.label}><h4>{group.label}</h4><div className="shortcut-rows">{SHORTCUT_ACTIONS.filter(action=>action.group===group.id).map(action=><div className="shortcut-row" key={action.id}><label htmlFor={`shortcut-${action.id}`}>{action.label}</label><span className="shortcut-controls"><input id={`shortcut-${action.id}`} className={`${capturing===action.id?'capturing':''} ${draft[action.id]?'':'unassigned'}`} aria-label={`${action.label} kısayolu`} aria-describedby="shortcut-key-help" readOnly disabled={blocked} value={formatShortcut(draft[action.id])} onFocus={()=>{setCapturing(action.id);setSaved(false);}} onBlur={()=>setCapturing(null)} onKeyDown={event=>{
      if(event.key==='Tab'&&!event.ctrlKey&&!event.altKey)return;
      event.preventDefault();event.stopPropagation();
      if(event.key==='Escape'){event.currentTarget.blur();return;}
      if(event.repeat||event.nativeEvent.isComposing||['Control','Shift','Alt','Meta','AltGraph'].includes(event.key))return;
      const combo=shortcutFromEvent(event.nativeEvent);
      const reason=combo?validateShortcut(combo):validateShortcut('');
      if(reason){setError(reason);setSaved(false);return;}
      assign(action.id,combo);
    }}/><button type="button" className="shortcut-clear" aria-label={`Kısayolu kaldır: ${action.label}`} title="Kısayolu kaldır" disabled={blocked||!draft[action.id]} onClick={()=>assign(action.id,'')}><X size={13}/></button></span></div>)}</div></section>)}
    <p className="shortcut-help" id="shortcut-key-help">{modifierWords()} ile bir harf, rakam ya da + - / [ ] , . ; ' Enter; isteğe bağlı Shift. Pano, geri alma ve sistem kısayolları korunur. Değişiklikler yalnız bu cihazda saklanır.</p>
    {(error||controller.error)?<p className="shortcut-error" role="alert">{error||controller.error}</p>:null}
    {controller.loading?<p className="shortcut-state" role="status">Kısayollar yükleniyor…</p>:saved?<p className="shortcut-state" role="status">Kısayollar bu cihaza kaydedildi.</p>:changed?<p className="shortcut-state" role="status">Kaydedilmemiş değişiklikler var.</p>:null}
    <div className="shortcut-actions"><button disabled={blocked} onClick={()=>{setDraft({...DEFAULT_SHORTCUTS});setError('');setSaved(false);}}><RotateCcw size={14}/>Varsayılanları kullan</button><button disabled={blocked||!changed||!!invalid||!!error} onClick={()=>void apply()}><Save size={14}/>{controller.saving?'Kaydediliyor…':'Kısayolları kaydet'}</button>{controller.error?<button disabled={controller.saving||controller.loading} onClick={()=>{setError('');setSaved(false);void controller.reload();}}>Yeniden yükle</button>:null}</div>
    <section className="shortcut-group shortcut-fixed" aria-label="Sabit kısayollar"><h4>Sabit kısayollar</h4><dl>{FIXED_SHORTCUTS.map(item=><div className="shortcut-row" key={item.keys}><dt>{item.label}</dt><dd><kbd>{item.keys}</kbd></dd></div>)}</dl></section>
  </div>;
}
