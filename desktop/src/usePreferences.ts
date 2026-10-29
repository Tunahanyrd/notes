import {useCallback,useEffect,useRef,useState} from 'react';
import {api} from './api';
import {loadSettingsPreferences,saveSettingsPreferences,type SettingsPreferences} from './SettingsPanel';
const keys=['theme','darkPages','galleryMode','sortAscending','toolsOpen','defaultZoom'] as const;
const signature=(value:SettingsPreferences)=>JSON.stringify(keys.map(key=>value[key]));
function parse(value:unknown):SettingsPreferences{
  const v=value as Record<string,unknown>|null;
  if(!v||v.version!==1||!['grid','list'].includes(String(v.galleryMode))||typeof v.sortAscending!=='boolean'||typeof v.toolsOpen!=='boolean'||![75,100,125,150].includes(v.defaultZoom as number))throw Error('preferences_invalid');
  if(v.theme!==undefined&&!['system','light','dark'].includes(String(v.theme))||v.darkPages!==undefined&&typeof v.darkPages!=='boolean')throw Error('preferences_invalid');
  return {theme:(v.theme??'system') as SettingsPreferences['theme'],darkPages:(v.darkPages??false) as boolean,galleryMode:v.galleryMode as SettingsPreferences['galleryMode'],sortAscending:v.sortAscending,toolsOpen:v.toolsOpen,defaultZoom:v.defaultZoom as SettingsPreferences['defaultZoom']};
}
/** SQLite owns device persistence. localStorage is only an origin-local fallback. */
export function usePreferences(authorized:boolean){
  const [preferences,setPreferences]=useState(loadSettingsPreferences);
  const [preferenceError,setError]=useState('');
  const desired=useRef(preferences),saved=useRef<string|null>(null),ready=useRef(false),writing=useRef(false),hydrating=useRef(false),mounted=useRef(true);
  const touched=useRef<Partial<SettingsPreferences>>({});const readController=useRef<AbortController|null>(null);
  const drain=useCallback(async()=>{
    if(!ready.current||writing.current||!mounted.current)return;
    writing.current=true;
    try{
      while(mounted.current&&signature(desired.current)!==saved.current){
        const sending={...desired.current};
        try{
          const response=parse(await api('/api/preferences','PUT',{version:1,...sending}));
          if(signature(response)!==signature(sending))throw Error('preferences_mismatch');
          saved.current=signature(response);if(mounted.current)setError('');
        }catch{if(mounted.current)setError('Tercihler bu oturumda uygulanıyor, ancak cihazda saklanamadı. Yeniden denemek için tercihinizi tekrar seçin.');break;}
      }
    }finally{writing.current=false;}
  },[]);
  const hydrate=useCallback(async()=>{
    if(!authorized||(hydrating.current&&!readController.current?.signal.aborted)||ready.current||!mounted.current)return;
    hydrating.current=true;const controller=new AbortController();readController.current=controller;
    try{
      const remote=parse(await api('/api/preferences','GET',undefined,controller.signal));
      if(!mounted.current||controller.signal.aborted)return;
      saved.current=signature(remote);desired.current={...remote,...touched.current};ready.current=true;touched.current={};
      setPreferences(desired.current);saveSettingsPreferences(desired.current);setError('');void drain();
    }catch{if(mounted.current&&!controller.signal.aborted)setError('Cihaz tercihleri okunamadı. Geçici görünüm kullanılıyor; bir tercihi seçtiğinizde yeniden denenecek.');}
    finally{if(readController.current===controller)hydrating.current=false;}
  },[authorized,drain]);
  useEffect(()=>{mounted.current=true;void hydrate();return()=>{mounted.current=false;readController.current?.abort();};},[hydrate]);
  const changePreferences=useCallback((next:SettingsPreferences,patch?:Partial<SettingsPreferences>)=>{
    if(patch)Object.assign(touched.current,patch);
    else for(const key of keys)if(next[key]!==desired.current[key])Object.assign(touched.current,{[key]:next[key]});
    desired.current=patch?{...desired.current,...patch}:{...next};setPreferences(desired.current);saveSettingsPreferences(desired.current);
    if(ready.current)void drain();else void hydrate();
  },[drain,hydrate]);
  return {preferences,changePreferences,preferenceError};
}
