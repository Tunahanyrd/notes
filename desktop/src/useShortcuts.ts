import {useCallback,useEffect,useRef,useState} from 'react';
import {api} from './api';

/**
 * Keyboard shortcuts of Windows Samsung Notes 4.3.831.0 whose action exists in
 * this client (docs/WINDOWS_SHORTCUTS.md), plus the Linux-only search/save.
 * Order and defaults mirror samsung_notes/shortcuts.py (schema 2).
 */
export const SHORTCUT_GROUPS=[{id:'general',label:'Genel'},{id:'view',label:'Not görünümü'},{id:'text',label:'Metin biçimlendirme'}] as const;
export type ShortcutGroup=typeof SHORTCUT_GROUPS[number]['id'];
export const SHORTCUT_ACTIONS=[
  {id:'newNote',label:'Yeni not oluştur',group:'general'},
  {id:'searchNotes',label:'Notlarda ara',group:'general'},
  {id:'saveNote',label:'Notu bu cihaza kaydet',group:'general'},
  {id:'zoomIn',label:'Yakınlaştır',group:'view'},
  {id:'zoomOut',label:'Uzaklaştır',group:'view'},
  {id:'readingMode',label:'Okuma modunu aç/kapat',group:'view'},
  {id:'fullScreen',label:'Tam ekranı aç/kapat',group:'view'},
  {id:'penSettings',label:'Kalem ayarlarını göster/gizle',group:'view'},
  {id:'pageSettings',label:'Sayfa ayarlarını aç/kapat',group:'view'},
  {id:'printNote',label:'Notu yazdır',group:'view'},
  {id:'bold',label:'Kalın',group:'text'},
  {id:'italic',label:'İtalik',group:'text'},
  {id:'underline',label:'Altı çizili',group:'text'},
  {id:'strikethrough',label:'Üstü çizili',group:'text'},
  {id:'alignLeft',label:'Sola hizala',group:'text'},
  {id:'alignCenter',label:'Ortala',group:'text'},
  {id:'alignRight',label:'Sağa hizala',group:'text'},
  {id:'numberedList',label:'Numaralı liste',group:'text'},
  {id:'bulletList',label:'Madde işaretli liste',group:'text'},
  {id:'checklist',label:'Onay kutusu',group:'text'},
  {id:'fontSmaller',label:'Yazı boyutunu küçült',group:'text'},
  {id:'fontLarger',label:'Yazı boyutunu büyüt',group:'text'},
  {id:'indent',label:'Girintiyi artır',group:'text'},
  {id:'outdent',label:'Girintiyi azalt',group:'text'},
] as const satisfies readonly {id:string;label:string;group:ShortcutGroup}[];
export type ShortcutAction=typeof SHORTCUT_ACTIONS[number]['id'];
export type ShortcutBindings=Record<ShortcutAction,string>;
export interface ShortcutDocument {schemaVersion:2;bindings:ShortcutBindings}
/**
 * macOS: the stored "Ctrl+" prefix means the platform's primary modifier, which
 * is Command (⌘) there; the physical Control key is left to macOS text
 * navigation (Ctrl+A/E/K…). Bindings stay one portable format on every OS.
 */
export const IS_MAC=typeof navigator!=='undefined'&&/Mac|iPhone|iPad|iPod/.test(navigator.platform||'');
export const DEFAULT_SHORTCUTS:Readonly<ShortcutBindings>={
  newNote:'Ctrl+N',searchNotes:'Ctrl+K',saveNote:'Ctrl+S',
  zoomIn:'Ctrl+Plus',zoomOut:'Ctrl+Minus',readingMode:'Alt+Enter',fullScreen:'Alt+F',penSettings:'Ctrl+T',pageSettings:'Alt+P',printNote:'Ctrl+Shift+P',
  bold:'Ctrl+B',italic:'Ctrl+I',underline:'Ctrl+U',strikethrough:'Ctrl+Slash',alignLeft:'Ctrl+L',alignCenter:'Ctrl+E',alignRight:'Ctrl+R',
  numberedList:'Ctrl+Shift+7',bulletList:'Ctrl+Shift+8',checklist:'Ctrl+Shift+C',fontSmaller:'Ctrl+BracketLeft',fontLarger:'Ctrl+BracketRight',indent:'Ctrl+M',outdent:'Ctrl+Shift+M',
};
/** Built in and not remappable (system, clipboard, undo, focus navigation). */
export const FIXED_SHORTCUTS=[
  {keys:'Tab / Shift+Tab',label:'Sonraki / önceki düğmeyi seç'},
  {keys:'Enter',label:'Okuma modundan düzenlemeye geç'},
  {keys:'Ctrl + fare tekerleği',label:'Yakınlaştır / uzaklaştır'},
  {keys:IS_MAC?'⌘Z / ⌘⇧Z':'Ctrl+Z / Ctrl+Y',label:'Geri al / yinele (metin ve çizim)'},
  {keys:'Menü tuşu / Shift+F10',label:'Seçili kartın sağ tık menüsü'},
  {keys:'Esc',label:'Menüyü veya pencereyi kapat'},
] as const;
// Clipboard, undo/redo, quit/close and browser chords stay with the system.
const RESERVED=new Set(['Ctrl+A','Ctrl+C','Ctrl+V','Ctrl+X','Ctrl+Z','Ctrl+Y','Ctrl+Shift+Z','Ctrl+Q','Ctrl+W','Ctrl+Shift+N','Ctrl+Shift+K','Ctrl+Shift+I','Ctrl+Shift+V']);
const CHORD=/^(?:Ctrl\+(?:Alt\+)?|Alt\+)(?:Shift\+)?(?:[A-Z0-9]|Plus|Minus|Slash|BracketLeft|BracketRight|Comma|Period|Quote|Semicolon|Enter)$/;
const SYMBOLS:Record<string,string>={Plus:'+',Minus:'-',Slash:'/',BracketLeft:'[',BracketRight:']',Comma:',',Period:'.',Quote:"'",Semicolon:';'};
const MAC_MODIFIERS:Record<string,string>={Ctrl:'⌘',Alt:'⌥',Shift:'⇧'};
/** Display form: "Ctrl+Shift+F" (Linux/Windows) or "⌘⇧F" (macOS). */
export function formatShortcut(value:string,mac=IS_MAC):string {
  if(!value)return 'Atanmadı';
  const parts=value.split('+');const key=parts.pop()!;
  if(mac)return parts.map(p=>MAC_MODIFIERS[p]??p).join('')+(SYMBOLS[key]??key);
  return [...parts,SYMBOLS[key]??key].join('+');
}
/** "Ctrl veya Alt" / "⌘ veya ⌥" for help and error texts. */
export function modifierWords(mac=IS_MAC):string {return mac?'⌘ veya ⌥':'Ctrl veya Alt';}
export function validateShortcut(value:string,mac=IS_MAC):string|null {
  if(!CHORD.test(value))return `${modifierWords(mac)} ile bir harf, rakam ya da + - / [ ] , . ; ' Enter tuşu kullanın. İsterseniz Shift ekleyebilirsiniz.`;
  if(RESERVED.has(value))return 'Bu birleşim sistem, pano veya geri alma için ayrılmış. Başka bir birleşim seçin.';
  return null;
}
export function validateBindings(value:unknown):string|null {
  if(!value||typeof value!=='object'||Array.isArray(value))return 'Kısayol ayarları okunamadı.';
  const bindings=value as Record<string,unknown>;
  if(Object.keys(bindings).length!==SHORTCUT_ACTIONS.length||SHORTCUT_ACTIONS.some(a=>typeof bindings[a.id]!=='string'))return 'Kısayol ayarları okunamadı.';
  const seen=new Map<string,string>();
  for(const action of SHORTCUT_ACTIONS){
    const binding=bindings[action.id] as string;
    if(!binding)continue;
    const invalid=validateShortcut(binding);if(invalid)return invalid;
    const previous=seen.get(binding);if(previous)return `${formatShortcut(binding)}, “${previous}” için kullanılıyor. Her işlem için farklı bir birleşim seçin.`;
    seen.set(binding,action.label);
  }
  return null;
}
export function readShortcutDocument(value:unknown):ShortcutBindings {
  const doc=value as Partial<ShortcutDocument>|null;
  if(!doc||doc.schemaVersion!==2||validateBindings(doc.bindings))throw new Error('Kısayol ayarları bu sürümle uyumlu değil.');
  const bindings={} as ShortcutBindings;for(const action of SHORTCUT_ACTIONS)bindings[action.id]=doc.bindings![action.id];
  return bindings;
}
// Character first (layout aware) for symbols; physical position for letters/digits, as Windows virtual keys.
const BY_KEY:Record<string,string>={'=':'Plus','-':'Minus','/':'Slash','[':'BracketLeft',']':'BracketRight',',':'Comma','.':'Period',"'":'Quote',';':'Semicolon'};
const BY_CODE:Record<string,string>={Equal:'Plus',NumpadAdd:'Plus',Minus:'Minus',NumpadSubtract:'Minus',Slash:'Slash',NumpadDivide:'Slash',BracketLeft:'BracketLeft',BracketRight:'BracketRight',Comma:'Comma',Period:'Period',Quote:'Quote',Semicolon:'Semicolon',Enter:'Enter',NumpadEnter:'Enter'};
export function shortcutFromEvent(event:Pick<KeyboardEvent,'key'|'code'|'ctrlKey'|'shiftKey'|'altKey'|'metaKey'|'getModifierState'>,mac=IS_MAC):string {
  // Primary modifier: Ctrl (Linux/Windows) or ⌘ (macOS); the other one never matches.
  const primary=mac?event.metaKey:event.ctrlKey,other=mac?event.ctrlKey:event.metaKey;
  if(other||event.getModifierState('AltGraph'))return '';
  let shift=event.shiftKey,key='';
  const physical=/^Key([A-Z])$/.exec(event.code)||/^Digit([0-9])$/.exec(event.code);
  // "+" is a shifted key on most layouts; Windows treats Ctrl + "+" the same way.
  if(event.key==='+'){key='Plus';shift=false;}
  else if(physical)key=physical[1];
  else if(BY_KEY[event.key])key=BY_KEY[event.key];
  else if(BY_CODE[event.code])key=BY_CODE[event.code];
  else if(/^[a-z0-9]$/i.test(event.key))key=event.key.toUpperCase();
  if(!key)return '';
  return `${primary?'Ctrl+':''}${event.altKey?'Alt+':''}${shift?'Shift+':''}${key}`;
}
// The rich editor's own formatting chords; when re-bound they must not fall through to native contenteditable formatting.
const NATIVE_FORMAT=new Set(['Ctrl+B','Ctrl+I','Ctrl+U']);
/** A handler returning `false` (synchronously) declines the key, e.g. formatting while the editor is not focused. */
export type ShortcutHandlers=Partial<Record<ShortcutAction,(()=>void|boolean|Promise<unknown>)|null|false|undefined>>;
export interface ShortcutsController {
  bindings:ShortcutBindings;loading:boolean;saving:boolean;loaded:boolean;error:string;
  save:(bindings:ShortcutBindings)=>Promise<boolean>;
  reload:()=>Promise<void>;
  label:(action:ShortcutAction)=>string;
}
/** Handlers are read at key time; a missing/false handler means the action is not available right now. */
export function useShortcuts({authorized=true,enabled,actions}:{authorized?:boolean;enabled:boolean;actions:ShortcutHandlers}):ShortcutsController {
  const [bindings,setBindings]=useState<ShortcutBindings>({...DEFAULT_SHORTCUTS});
  const [loading,setLoading]=useState(true),[loaded,setLoaded]=useState(false),[saving,setSaving]=useState(false),[error,setError]=useState('');
  const mounted=useRef(true),inFlight=useRef(false),generation=useRef(0);
  const latest=useRef({enabled,actions,bindings,loaded});latest.current={enabled,actions,bindings,loaded};
  const reload=useCallback(async()=>{
    if(!authorized||inFlight.current)return;
    const current=++generation.current;setLoading(true);setLoaded(false);setError('');
    try{
      const value=readShortcutDocument(await api<ShortcutDocument>('/api/shortcuts'));
      if(mounted.current&&generation.current===current){setBindings(value);setLoaded(true);}
    }catch{if(mounted.current&&generation.current===current){setError('Kısayollar yüklenemedi. Yeniden yükleyin.');setLoaded(false);}}
    finally{if(mounted.current&&generation.current===current)setLoading(false);}
  },[authorized]);
  useEffect(()=>{mounted.current=true;if(authorized)void reload();else setLoading(false);return()=>{mounted.current=false;generation.current++;};},[authorized,reload]);
  const save=useCallback(async(candidate:ShortcutBindings)=>{
    const invalid=validateBindings(candidate);
    if(invalid){setError(invalid);return false;}
    if(!authorized||!loaded||loading||inFlight.current)return false;
    inFlight.current=true;setSaving(true);setError('');
    const frozen={...candidate};
    try{
      const result=readShortcutDocument(await api<ShortcutDocument>('/api/shortcuts','PUT',{schemaVersion:2,bindings:frozen}));
      if(SHORTCUT_ACTIONS.some(a=>result[a.id]!==frozen[a.id]))throw new Error('Saved shortcuts differ.');
      if(mounted.current)setBindings(result);
      return true;
    }catch{
      if(mounted.current){setError('Kısayolların kaydı doğrulanamadı. Yeniden yükleyerek cihazdaki ayarları kontrol edin.');setLoaded(false);}
      return false;
    }finally{inFlight.current=false;if(mounted.current)setSaving(false);}
  },[authorized,loaded,loading]);
  useEffect(()=>{
    // Capture phase: runs before the editor (ProseMirror) so a remapped chord wins.
    const key=(event:KeyboardEvent)=>{
      const state=latest.current;
      if(!state.enabled||!state.loaded||event.defaultPrevented||event.repeat||event.isComposing)return;
      const combo=shortcutFromEvent(event);
      if(!combo)return;
      const action=SHORTCUT_ACTIONS.find(a=>state.bindings[a.id]===combo);
      const handler=action?state.actions[action.id]:null;
      let result:void|boolean|Promise<unknown>=false;
      try{result=handler?handler():false;}catch{result=undefined;if(mounted.current)setError('Kısayol işlemi tamamlanamadı.');}
      if(result===false){
        if(NATIVE_FORMAT.has(combo)&&event.target instanceof HTMLElement&&event.target.isContentEditable)event.preventDefault();
        return;
      }
      event.preventDefault();event.stopPropagation();
      if(result instanceof Promise)result.catch(()=>{if(mounted.current)setError('Kısayol işlemi tamamlanamadı.');});
    };
    window.addEventListener('keydown',key,true);
    return()=>window.removeEventListener('keydown',key,true);
  },[]);
  return {bindings,loading,saving,loaded,error,save,reload,label:action=>formatShortcut(bindings[action])};
}
