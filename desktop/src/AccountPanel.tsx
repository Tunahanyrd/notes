import {useId, useRef, useState} from 'react';
import {ArrowUpRight, Check, Cloud, CloudOff, LoaderCircle, LogOut, ShieldCheck} from 'lucide-react';
import type {Status} from './types';
import './account-panel.css';

export interface AccountPanelProps {
  status: Status | null;
  busy: boolean;
  onLogin: () => void | Promise<unknown>;
  onDisconnect: () => void | Promise<unknown>;
  error?: string;
}

/** Account state comes exclusively from the local engine; no credentials live here. */
export function AccountPanel({status, busy, onLogin, onDisconnect, error}:AccountPanelProps) {
  const headingId=useId();
  const [action,setAction]=useState<'login'|'disconnect'|null>(null);
  const [actionError,setActionError]=useState('');
  const inFlight=useRef(false);
  const connected=!!status?.connected;
  const pending=!!status?.loginPending;
  const disabled=busy||!!action||pending||!status;
  const failure=actionError||error||status?.loginError||'';
  const perform=async(kind:'login'|'disconnect')=>{
    if(disabled||inFlight.current)return;
    inFlight.current=true;setAction(kind);setActionError('');
    try {await (kind==='login'?onLogin():onDisconnect());}
    catch {setActionError(kind==='login'?'Giriş başlatılamadı. Bağlantınızı kontrol edip yeniden deneyin.':'Bağlantı kesilemedi. Hesap durumunu kontrol edip yeniden deneyin.');}
    finally {inFlight.current=false;setAction(null);}
  };
  const title=!status?'Hesap durumu yükleniyor':pending?'Giriş tamamlanıyor':connected?'Samsung hesabı bağlı':'Samsung hesabınızla bağlanın';
  return <section className="account-panel" aria-labelledby={headingId} aria-busy={!status||!!action}>
    <div className="account-panel-heading">
      <span className={`account-symbol${connected?' connected':''}`} aria-hidden="true">{!status||pending?<LoaderCircle size={24} className="spin"/>:connected?<Cloud size={25}/>:<CloudOff size={25}/>}</span>
      <div><h3 id={headingId}>{title}</h3><p>{connected?(status?.accountLabel||'Samsung hesabı'):'Notlarınıza cihazlarınızdan ulaşın.'}</p></div>
      {connected&&!pending?<span className="account-connected"><Check size={13} aria-hidden="true"/>Bağlı</span>:null}
    </div>
    {pending?<div className="account-pending" role="status"><p>Tarayıcıda açılan Samsung girişini tamamlayın.</p></div>:connected?null:<p className="account-description">Giriş sistem tarayıcınızda, Samsung’un sayfasında yapılır. Parolanızı bu uygulamaya girmeniz gerekmez. Hesap bağlamadan da yerel notlarla çalışabilirsiniz.</p>}
    {failure&&!pending?<p className="account-error" role="alert">{failure}</p>:null}
    <div className="account-panel-actions">
      {connected?<button className="account-disconnect" disabled={disabled} onClick={()=>void perform('disconnect')} title="Bu uygulamanın hesap bağlantısını keser; buluttaki ve bu cihazdaki notları silmez.">{action==='disconnect'?<LoaderCircle size={15} className="spin"/>:<LogOut size={15}/>}Bağlantıyı kes</button>:<button className="account-login" disabled={disabled} onClick={()=>void perform('login')}>{pending||action==='login'?<LoaderCircle size={16} className="spin"/>:<ArrowUpRight size={16}/>}<span>{pending?'Tarayıcıda giriş bekleniyor':action==='login'?'Giriş açılıyor…':failure?'Yeniden giriş yap':'Samsung hesabıyla bağlan'}</span></button>}
    </div>
    <p className="account-privacy"><ShieldCheck size={14} aria-hidden="true"/><span>Parola bu uygulamada tutulmaz. Hesap bağlantısını kesmek Samsung hesabınızı veya notlarınızı silmez.</span></p>
  </section>;
}
