import {Select} from './Select';
import {useRef, useState, type ReactNode, type Ref} from 'react';
import {ArrowLeft, Check,  Grid2X2, List, RefreshCw} from 'lucide-react';
import type {Status} from './types';
import {version as APP_VERSION} from '../package.json';
import './settings-panel.css';

const OPEN_SOURCE:readonly {name:string;license:string;note?:string}[]=[
  {name:'sdocx kalem çizici (tools/ink-renderer)',license:'GPL-3.0',note:'Fountain kalem çizgilerini önizlemede çizer. GNU Genel Kamu Lisansı 3.0 altındadır; kaynak kodu ve lisans metni tools/ink-renderer/ klasöründedir (upstream commit 41e9c1099b29a48dc7320501573d7c4858f09303).'},
  {name:'React, React DOM',license:'MIT'},
  {name:'Tiptap, ProseMirror',license:'MIT'},
  {name:'Lucide simgeleri',license:'ISC'},
  {name:'Tauri',license:'MIT veya Apache-2.0'},
  {name:'Poppler, ImageMagick',license:'GPL-2.0+ / ImageMagick',note:'Sistemde kurulu araçlar; PDF sayfası çizimi ve dışa aktarma için ayrı süreçte çalıştırılır, uygulamayla dağıtılmaz.'},
];

export interface SettingsPreferences {
  theme:'system'|'light'|'dark';
  darkPages:boolean;
  galleryMode:'grid'|'list';
  sortAscending:boolean;
  toolsOpen:boolean;
  defaultZoom:75|100|125|150;
}
export const SETTINGS_STORAGE_KEY='samsung-notes.preferences.v1';
export const DEFAULT_SETTINGS:Readonly<SettingsPreferences>={theme:'system',darkPages:false,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100};
/** Device-local display preferences only. Never put account data in this key. */
function loadLocalSettingsPreferences():SettingsPreferences {
  try {
    const saved:unknown=JSON.parse(localStorage.getItem(SETTINGS_STORAGE_KEY)||'null');
    if(!saved||typeof saved!=='object'||!('version' in saved)||saved.version!==1)return {...DEFAULT_SETTINGS};
    const p=saved as Record<string,unknown>;
    return {theme:p.theme==='light'||p.theme==='dark'?p.theme:'system',darkPages:p.darkPages===true,galleryMode:p.galleryMode==='list'?'list':'grid',sortAscending:typeof p.sortAscending==='boolean'?p.sortAscending:DEFAULT_SETTINGS.sortAscending,toolsOpen:typeof p.toolsOpen==='boolean'?p.toolsOpen:DEFAULT_SETTINGS.toolsOpen,defaultZoom:[75,100,125,150].includes(p.defaultZoom as number)?p.defaultZoom as SettingsPreferences['defaultZoom']:100};
  } catch {return {...DEFAULT_SETTINGS};}
}
export function loadSettingsPreferences():SettingsPreferences {
  const settings=loadLocalSettingsPreferences();
  const cached=document.cookie.match(/(?:^|;\s*)notes_theme_v1=(system|light|dark)\.([01])(?:;|$)/);
  return cached?{...settings,theme:cached[1] as SettingsPreferences['theme'],darkPages:cached[2]==='1'}:settings;
}
export function saveSettingsPreferences(preferences:SettingsPreferences):boolean {
  try {localStorage.setItem(SETTINGS_STORAGE_KEY,JSON.stringify({version:1,...preferences}));document.cookie=`notes_theme_v1=${preferences.theme}.${preferences.darkPages?1:0}; Path=/; Max-Age=31536000; SameSite=Strict`;return true;}
  catch {return false;}
}
export interface SettingsPanelProps {
  status:Status|null;
  foldersInitialized:boolean;
  offlineView:boolean;
  busy:boolean;
  error?:string;
  preferenceError?:string;
  onClose:()=>void;
  onSync:()=>void|Promise<unknown>;
  sectionRef:Ref<HTMLElement>;
  accountSlot:ReactNode;
  shortcutSlot?:ReactNode;
  preferences:SettingsPreferences;
  onPreferencesChange:(preferences:SettingsPreferences,patch?:Partial<SettingsPreferences>)=>void;
}

export function SettingsPanel({status,busy,error,preferenceError,onClose,onSync,sectionRef,accountSlot,shortcutSlot,preferences,onPreferencesChange}:SettingsPanelProps) {
  const [syncBusy,setSyncBusy]=useState(false);
  const [syncError,setSyncError]=useState('');
  const requestInFlight=useRef(false);
  const syncing=syncBusy||status?.sync.state==='syncing';
  const change=(patch:Partial<SettingsPreferences>)=>onPreferencesChange({...preferences,...patch},patch);
  const sync=async()=>{
    if(!status?.connected||busy||syncing||requestInFlight.current)return;
    requestInFlight.current=true;setSyncBusy(true);setSyncError('');
    try {await onSync();}catch {setSyncError('Eşitleme başlatılamadı. Bağlantınızı kontrol edip yeniden deneyin.');}
    finally {requestInFlight.current=false;setSyncBusy(false);}
  };
  return <div className="modal-backdrop settings-backdrop" onClick={onClose}>
    <section ref={sectionRef} className="settings-modal settings-page" role="dialog" aria-modal="true" aria-labelledby="settings-title" onClick={event=>event.stopPropagation()}>
      <header className="settings-page-header"><button className="icon-button" aria-label="Ayarları kapat" onClick={onClose}><ArrowLeft size={22}/></button><h2 id="settings-title">Ayarlar</h2></header>
      <div className="settings-page-scroll">
        <section className="settings-group" aria-labelledby="settings-account-heading"><h3 id="settings-account-heading">Bulut eşitlemesi</h3>{accountSlot}
          <div className="settings-card settings-sync-card">
            <div className="settings-option"><div><span className="settings-option-title">Şimdi eşitle</span></div><button className="settings-sync-button" disabled={!status?.connected||busy||syncing} onClick={()=>void sync()}><RefreshCw size={16} className={syncing?'spin':''}/>Şimdi eşitle</button></div>
            {(syncError||error||status?.sync.error)&&<p className="settings-inline-error" role="alert">{syncError||error||status?.sync.error}</p>}
          </div>
        </section>
        <section className="settings-group" aria-labelledby="settings-general-heading"><h3 id="settings-general-heading">Genel</h3>{preferenceError&&<p className="settings-inline-error" role="alert">{preferenceError}</p>}<div className="settings-card">
          <div className="settings-option"><span><span className="settings-option-title">Tema</span><span className="settings-option-description">Uygulamanın görünümünü seçin.</span></span><Select label="Tema" value={preferences.theme} onChange={value=>change({theme:value as SettingsPreferences['theme']})} options={[{value:'system',label:'Sistem'},{value:'light',label:'Açık'},{value:'dark',label:'Koyu'}]}/></div>
          <label className="settings-option"><span><span className="settings-option-title">Not sayfalarını koyu göster</span><span className="settings-option-description">Sayfaları yalnızca ekranda koyu gösterir.</span></span><input className="settings-toggle" type="checkbox" role="switch" aria-label="Not sayfalarını koyu göster" checked={preferences.darkPages} onChange={event=>change({darkPages:event.target.checked})}/></label>
          <div className="settings-option settings-view-option"><div><span className="settings-option-title">Not listesi</span><span className="settings-option-description">Notlarınızın nasıl görüneceğini seçin.</span></div><div className="settings-view-choices" role="group" aria-label="Not listesi görünümü">{(['grid','list'] as const).map(mode=><button key={mode} aria-pressed={preferences.galleryMode===mode} onClick={()=>change({galleryMode:mode})}>{mode==='grid'?<Grid2X2 size={23}/>:<List size={23}/>}<span>{mode==='grid'?'Izgara':'Liste'}</span>{preferences.galleryMode===mode&&<Check size={13} className="settings-choice-check"/>}</button>)}</div></div>
          <div className="settings-option"><span><span className="settings-option-title">Değiştirilme tarihine göre sırala</span><span className="settings-option-description">Not listesinin sıralama yönü.</span></span><Select label="Not sıralaması" value={preferences.sortAscending?'oldest':'newest'} onChange={value=>change({sortAscending:value==='oldest'})} options={[{value:'newest',label:'En yeni önce'},{value:'oldest',label:'En eski önce'}]}/></div>
          <label className="settings-option"><span><span className="settings-option-title">Metin araçları</span><span className="settings-option-description">Düzenleyicide biçimlendirme ve kalem panelini göster.</span></span><input className="settings-toggle" type="checkbox" role="switch" aria-label="Metin araçlarını göster" checked={preferences.toolsOpen} onChange={event=>change({toolsOpen:event.target.checked})}/></label>
          <div className="settings-option"><span><span className="settings-option-title">Varsayılan yakınlaştırma</span><span className="settings-option-description">Sayfanın ekrandaki boyutu. Not içeriğini değiştirmez.</span></span><Select label="Varsayılan yakınlaştırma" value={String(preferences.defaultZoom)} onChange={value=>change({defaultZoom:Number(value) as SettingsPreferences['defaultZoom']})} options={[75,100,125,150].map(value=>({value:String(value),label:`%${value}`}))}/></div>
        </div><p className="settings-group-help">Bu tercihler yalnız bu cihazda saklanır. Not kilitleme, varsayılan not stili ve not geri yükleme bu sürümde yok.</p></section>
        {shortcutSlot?<section className="settings-group" aria-labelledby="settings-shortcuts-heading"><h3 id="settings-shortcuts-heading">Klavye kısayolları</h3>{shortcutSlot}</section>:null}
        <section className="settings-group" aria-labelledby="settings-privacy-heading"><h3 id="settings-privacy-heading">Gizlilik</h3><div className="settings-card">
          <div className="settings-option"><div><span className="settings-option-title">Hesap parolası</span><span className="settings-option-description">Giriş Samsung’un kendi sayfasında, sistem tarayıcınızda yapılır. Parolanız bu uygulamaya girilmez ve saklanmaz.</span></div></div>
          <div className="settings-option"><div><span className="settings-option-title">Bu cihazdaki veriler</span><span className="settings-option-description">İndirilen notlar, taslaklar ve ayarlar yalnız bu cihazdaki uygulama klasöründe tutulur ve yalnız Samsung Cloud ile eşitlenir.</span></div></div>
        </div></section>
        <section className="settings-about" aria-labelledby="settings-about-heading">
          <h3 id="settings-about-heading">Linux için Notes</h3>
          <p>Sürüm {APP_VERSION}</p>
          <p>Samsung Notes ile çalışan bağımsız Linux istemcisi. Samsung’un resmî uygulaması değildir.</p>
          
          <details className="settings-licenses"><summary>Açık kaynak lisansları</summary><ul>{OPEN_SOURCE.map(item=><li key={item.name}><strong>{item.name}</strong> · {item.license}{item.note?<span>{item.note}</span>:null}</li>)}</ul></details>
        </section>
      </div>
    </section>
  </div>;
}
