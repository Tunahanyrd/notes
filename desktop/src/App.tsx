import {Select} from './Select';
import {useCallback, useEffect, useRef, useState,type MouseEvent,type KeyboardEvent as ReactKeyboardEvent} from 'react';
import {ArrowLeft, ArrowUp, ArrowDown, BookOpen, Check, ChevronRight, CircleCheck, Cloud, CloudOff, FilePenLine, FileText, Folder, Grid3X3, Heart, List, LoaderCircle, Maximize2, Menu, Minus, MoreVertical, NotebookPen, PanelTop, Plus, Printer, Save, Search, Share2, SquarePen, ShieldAlert, Trash2, X} from 'lucide-react';
import {TextTools, WinIcon} from './WindowsChrome';
import {RichEditor} from './RichEditor';
import {NoteThumbnail} from './NoteThumbnail';
import {ContextMenu,type ContextTarget,type ContextAction} from './ContextMenu';
import {PageSorter, type SorterPage} from './PageSorter';
import {ShapeLayer,ShapePanel,useShapes} from './ShapeTool';
import {InkEditor,useInkDrafts,canAppendInk,inkStyles,canDrawInk,canEraseInk,canSelectInk,inkToolReason,pageDrawMode} from './InkEditor';
import {AccountPanel} from './AccountPanel';
import {SettingsPanel} from './SettingsPanel';
import {usePreferences} from './usePreferences';
import './windows-parity.css';
import {useShortcuts,type ShortcutHandlers} from './useShortcuts';
import {runFormat,type FormatCommand} from './editorCommands';
import {KeyboardShortcuts} from './KeyboardShortcuts';
import {assertEditorModel,type EditorModel} from './editorModel';
import type {Editor} from '@tiptap/react';
import {PageStack,usePageManifest} from './PageStack';
import {FolderPicker,type FolderOption} from './FolderPicker';
import {useNoteDrag} from './useNoteDrag';
import {api, ApiError, initializeApi, notePath, request} from './api';
import type {Filter, Note, NoteFolder, Status} from './types';

type OrganizePatch = {favorite:boolean} | {trashed:boolean} | {folderId:string|null};
type FolderDialog = {kind:'create'|'rename'|'move'|'trash'|'restore';folder?:NoteFolder};
type Draft = {title:string;body:string;revision:number;dirty:boolean;editor?:EditorModel;saveError?:string};
interface EditorProjection {schemaVersion:1;capabilities:{titleEdit:boolean;textEdit:boolean;richStyle:boolean;inkEdit:boolean};model:EditorModel|null;reason:string|null;revision:number}
const navItems = [{id:'all',label:'Tüm notlar',icon:Folder},{id:'favorites',label:'Favoriler',icon:Heart},{id:'conflicts',label:'Çakışmalar',icon:ShieldAlert},{id:'trash',label:'Çöp kutusu',icon:Trash2}] as const;
const syncLabels:Record<Note['syncState'],string> = {synced:'Bulutla eşitlendi',pending:'Buluta gönderilmeyi bekliyor',local:'Bu cihazda kayıtlı',conflict:'İki farklı sürüm var',error:'Eşitleme tamamlanamadı',uncertain:'Eşitleme kontrol ediliyor'};
// The year is shown whenever it is not the current one ("30 Haz 2025").
const otherYear = (value:number) => new Date(value).getFullYear()!==new Date().getFullYear();
const date = (value:number|null, detailed=false) => value ? new Intl.DateTimeFormat('tr-TR', {...(detailed ? {day:'numeric',month:'long',hour:'2-digit',minute:'2-digit'} as const : {day:'numeric',month:'short'} as const), ...(otherYear(value)?{year:'numeric'} as const:{})}).format(value) : '';
const message = (e:unknown) => e instanceof Error ? e.message : 'İşlem tamamlanamadı.';

// Samsung clients name untitled notes by kind and creation day (MM/DD);
// shown only, never written back as a title.
// Samsung keeps trashed items 30 days (Windows NoteTrashManager "Remove Deleted Notes Over 30 Days").
const TRASH_DAYS=30,DAY=86400000;
function daysLeft(trashedAt:number|null|undefined){return trashedAt&&trashedAt>0?Math.max(0,Math.ceil((trashedAt+TRASH_DAYS*DAY-Date.now())/DAY)):null;}
function untitledName(note:Note){const day=note.createdAt??note.modifiedAt;const d=day?new Date(day):null;const stamp=d?[d.getDate(),d.getMonth()+1].map(n=>String(n).padStart(2,'0')).join('.')+(otherYear(d.getTime())?'.'+d.getFullYear():''):'';return `${!note.editable&&!(note.preview||'').trim()?'El yazısı notu':'Metin notu'}${stamp?' '+stamp:''}`;}
export default function App() {
  const [authorized] = useState(()=>{try{return initializeApi();}catch{return false;}});
  const {preferences,changePreferences,preferenceError}=usePreferences(authorized);
  const {galleryMode,sortAscending,toolsOpen,defaultZoom}=preferences;
  useEffect(()=>{const media=window.matchMedia('(prefers-color-scheme: dark)');const apply=()=>{document.documentElement.dataset.theme=preferences.theme==='system'?(media.matches?'dark':'light'):preferences.theme;document.documentElement.dataset.darkPages=String(preferences.darkPages);};apply();media.addEventListener('change',apply);return()=>media.removeEventListener('change',apply);},[preferences.theme,preferences.darkPages]);
  const setGalleryMode=(galleryMode:'grid'|'list')=>changePreferences({...preferences,galleryMode},{galleryMode});
  const setSortAscending=(sortAscending:boolean)=>changePreferences({...preferences,sortAscending},{sortAscending});
  const setToolsOpen=(toolsOpen:boolean)=>changePreferences({...preferences,toolsOpen},{toolsOpen});
  const [accountError,setAccountError]=useState('');
  const [syncError,setSyncError]=useState('');
  const [status,setStatus] = useState<Status|null>(null);
  const [notes,setNotes] = useState<Note[]>([]);
  const [filter,setFilter] = useState<Filter>('all');
  const [query,setQuery] = useState('');
  const [folders,setFolders] = useState<NoteFolder[]>([]);
  const [trashedFolders,setTrashedFolders] = useState<NoteFolder[]>([]);
  const [libraryCounts,setLibraryCounts] = useState<{all:number;trash:number}|null>(null);
  const [expandedFolders,setExpandedFolders] = useState<Set<string>>(()=>new Set());
  const [contextMenu,setContextMenu]=useState<ContextTarget|null>(null);
  const [folderDialog,setFolderDialog] = useState<FolderDialog|null>(null);
  const [folderName,setFolderName] = useState('');
  const [folderParent,setFolderParent] = useState('');
  const [foldersInitialized,setFoldersInitialized] = useState(false);
  const [folderId,setFolderId] = useState<string|null>(null);
  const [trashFolderId,setTrashFolderId] = useState<string|null>(null);
  const trashReadOnly=filter==='trash'&&!!trashFolderId;
  const [foldersOpen,setFoldersOpen] = useState(true);
  const [moveTargets,setMoveTargets] = useState<string[]>([]);
  // null: nothing chosen yet; '' is "Klasörsüz" (library root).
  const [moveFolder,setMoveFolder] = useState<string|null>(null);
  const [pollError,setPollError] = useState('');
  const [selected,setSelected] = useState<string|null>(null);
  const [detail,setDetail] = useState<Note|null>(null);
  const [editorInfo,setEditorInfo] = useState<{id:string;projection:EditorProjection}|null>(null);
  const editorInfoRef=useRef(editorInfo);editorInfoRef.current=editorInfo;
  const [richEditor,setRichEditor] = useState<Editor|null>(null);
  const [drafts,setDrafts] = useState<Record<string,Draft>>({});
  const draftsRef = useRef(drafts); draftsRef.current = drafts;
  const [listLoading,setListLoading] = useState(true);
  const [detailLoading,setDetailLoading] = useState(false);
  const [error,setError] = useState('');
  const [thumbnailRefresh,setThumbnailRefresh]=useState(0);
  useEffect(()=>setThumbnailRefresh(value=>value+1),[status?.connected,status?.accountLabel]);
  const [notice,setNotice] = useState('');
  const [busy,setBusy] = useState<string|null>(null);
  const [saving,setSaving] = useState<string|null>(null);
  const savingRef = useRef(new Set<string>());
  const [settings,setSettings] = useState(false);
  const [sidebar,setSidebar] = useState(false);
  const [focusMode,setFocusMode] = useState(false);
  const [drawerCollapsed,setDrawerCollapsed] = useState(false);
  const [viewMenu,setViewMenu] = useState(false);
  const [noteMenu,setNoteMenu] = useState(false);
  const [selectionMode,setSelectionMode] = useState(false);
  const [selectedCards,setSelectedCards] = useState(new Set<string>());
  const [readingMode,setReadingMode] = useState(false);
  const [documentSearch,setDocumentSearch] = useState(false);
  const [zoom,setZoom] = useState<number>(defaultZoom);
  useEffect(()=>{setZoom(defaultZoom);},[selected,defaultZoom]);

  const [imageFailed,setImageFailed]=useState(false);
  const bodyCapabilityBlocked=!!detail&&editorInfo?.id===detail.id&&(!editorInfo.projection.capabilities.textEdit||!editorInfo.projection.capabilities.richStyle);
  const richAvailable=!!detail&&editorInfo?.id===detail.id&&!!editorInfo.projection.capabilities.textEdit&&!!editorInfo.projection.capabilities.richStyle&&!!drafts[detail.id]?.editor;
  const pagePreview=usePageManifest(detail?.id??null,detail?.revision,focusMode&&!!detail&&!detail.editable&&detail.source==='cloud');
  const [pageJump,setPageJump]=useState<{index:number;seq:number}|null>(null);
  const [shareMenu,setShareMenu]=useState(false);
  type PageStyleInfo={revision:number;backgroundArgb:number|null;editable:boolean;reason:string|null;pending?:boolean;palette?:{argb:number;name:string}[][];templates?:{id:string;name:string;pageCount:number;inNote?:boolean}[]};
  const [styleMenu,setStyleMenu]=useState<PageStyleInfo|'loading'|null>(null);
  useEffect(()=>setStyleMenu(null),[selected]);
  type PageOpsInfo={revision:number;editable:boolean;reason:string|null;pending?:boolean;canAdd?:boolean;pages:SorterPage[]};
  const [sorter,setSorter]=useState<PageOpsInfo|null>(null);
  useEffect(()=>setSorter(null),[selected]);
  const pdfInput=useRef<HTMLInputElement|null>(null);
  const [insertMenu,setInsertMenu]=useState(false);
  useEffect(()=>setInsertMenu(false),[selected]);
  useEffect(()=>setShareMenu(false),[selected]);
  // Pen/page notes whose text is editable open on their pages (handwriting
  // visible); "Metni düzenle" switches this note to the text editor.
  const [textView,setTextView]=useState<string|null>(null);
  const pagesReady=pagePreview.phase==='ready'&&!imageFailed;
  const showingPage=pagesReady&&!(richAvailable&&textView===detail?.id);
  // Text view of a handwriting note: the pen button leads back to the pages.
  const inTextOfPenNote=pagesReady&&richAvailable&&textView===detail?.id;
  const ink=useInkDrafts(detail,focusMode&&!!detail&&!detail.editable&&detail.source==='cloud',note=>{acceptMutation(note);void refreshStatus().catch(()=>{});},{onSaveErase:async request=>(await api<{note:Note}>(notePath(request.noteId)+'/ink-erase','PUT',{expectedRevision:request.expectedRevision,erase:request.erase})).note});
  const inkCaptureRef=useRef(ink.captureInProgress);inkCaptureRef.current=ink.captureInProgress;
  // Shapes (Şekil): own outbox job per shape; blocked while pen work is unsaved or syncing.
  const shapes=useShapes(detail,focusMode&&!!detail&&!detail.editable&&detail.source==='cloud',trashReadOnly?'Bu klasördeki notlar salt okunur.':detail&&(ink.hasDraft(detail.id)||ink.anySaving)?'Çizimler kaydedilirken şekil eklenemez.':'',note=>{acceptMutation(note);void refreshStatus().catch(()=>{});});
  useEffect(()=>{if(ink.drawing)shapes.setActive(false);},[ink.drawing]);
  const inkDirty=!!ink.draft?.strokes.length;
  const inkPending=!!ink.projection?.pendingStrokes.length;
  // Only a hint now: an explicitly partial page image (complete:false). Canvas
  // caches carry no flag and are Samsung's own full rendering.
  const inkBaselineComplete=pagePreview.pages.find(page=>page.index===pagePreview.pageIndex)?.complete!==false;
  useEffect(()=>{setImageFailed(false);},[selected]);
  useEffect(()=>setImageFailed(false),[pagePreview.phase]);
  const bodyRef=useRef<HTMLTextAreaElement>(null);
  const pageSelectorRef=useRef<HTMLButtonElement>(null);
  const canSelectPage=!detail?.editable&&pagePreview.phase==='ready'&&!imageFailed&&pagePreview.pages.length>0;
  const searchRef = useRef<HTMLInputElement>(null);
  const modalRef = useRef<HTMLElement>(null);
  const listVersion = useRef(0);
  const selectedRef=useRef(selected);selectedRef.current=selected;
  const detailRef=useRef(detail);detailRef.current=detail;
  const deferredDetail=useRef<{note:Note;projection:EditorProjection|null}|null>(null);
  const focusRef=useRef(focusMode);focusRef.current=focusMode;
  const mounted = useRef(true);
  useEffect(()=>{mounted.current=true;return()=>{mounted.current=false;};},[]);

  const acceptDetail = useCallback((note:Note,projection:EditorProjection|null)=>{
    if(!mounted.current || selectedRef.current!==note.id || savingRef.current.has(note.id)||(detailRef.current?.id===note.id&&detailRef.current.revision>note.revision))return;
    setDetail(previous=>previous?.id===note.id&&previous.revision>note.revision?previous:note);
    setEditorInfo(projection?{id:note.id,projection}:null);
    const editableModel=projection?.capabilities.textEdit&&projection?.capabilities.richStyle?projection.model:null;
    // A draft marked dirty whose content equals this revision has nothing to save: it is clean again.
    const same=(draft:Draft)=>draft.revision===note.revision&&draft.title===note.title&&(draft.editor?!!editableModel&&JSON.stringify(draft.editor)===JSON.stringify(editableModel):!editableModel&&draft.body===(note.body??''));
    setDrafts(previous=>previous[note.id]?.dirty&&same(previous[note.id])?{...previous,[note.id]:{...previous[note.id],dirty:false,saveError:undefined}}:previous[note.id]?.dirty||previous[note.id]?.revision>note.revision?previous:{...previous,[note.id]:{title:note.title,body:editableModel?.text??note.body??'',revision:note.revision,dirty:false,...(editableModel?{editor:editableModel}:{})}});
  },[]);
  const readDetail=useCallback(async(id:string,signal:AbortSignal)=>{
    const {note}=await api<{note:Note}>(notePath(id),'GET',undefined,signal);
    const existing=editorInfoRef.current,prior=detailRef.current;
    // Sync can replace a cached projection without advancing the local revision.
    const sameState=prior?.id===id&&prior.syncState===note.syncState&&prior.metadataSync?.state===note.metadataSync?.state&&prior.trashed===note.trashed&&prior.contentLoaded===note.contentLoaded;
    let projection:EditorProjection|null=existing?.id===id&&existing.projection.revision===note.revision&&sameState?existing.projection:null;
    try{
      const result=projection??await api<EditorProjection>(notePath(id)+'/editor','GET',undefined,signal);
      if(result.schemaVersion===1&&result.capabilities&&result.revision===note.revision){
        projection={...result,reason:result.reason?(result.capabilities.textEdit?'Bu notta yazı biçimlendirme şimdilik kullanılamıyor.':'Bu notun metni şimdilik burada düzenlenemiyor.'):null};
        if(result.model){try{assertEditorModel(result.model);}catch(e){projection={...result,model:null,capabilities:{...result.capabilities,textEdit:false,richStyle:false},reason:'Bu notun metni şimdilik burada düzenlenemiyor.'};}}
      }
    }catch(e){if(signal.aborted)throw e;}
    // A transient projection failure must never turn an existing rich draft into
    // a plain-text PUT. Keep the draft and its revision until the model is read.
    if(!projection&&draftsRef.current[id]?.editor)throw new Error('Notun biçimli içeriği şu anda okunamadı. Taslağınız korunuyor.');
    if(!signal.aborted){
      if(detailRef.current?.id===id&&inkCaptureRef.current())deferredDetail.current={note,projection};
      else acceptDetail(note,projection);
    }
  },[acceptDetail]);
  useEffect(()=>{
    if(ink.capturing)return;
    const deferred=deferredDetail.current;if(!deferred)return;
    deferredDetail.current=null;
    if(selectedRef.current===deferred.note.id)acceptDetail(deferred.note,deferred.projection);
  },[ink.capturing,acceptDetail]);
  const refreshList = useCallback(async(signal?:AbortSignal)=>{
    const version=++listVersion.current;
    const activeFolder=filter==='trash'?trashFolderId:folderId;
    const url=(kind:Filter)=>`/api/notes?filter=${kind}&q=${encodeURIComponent(query)}${activeFolder?`&folder=${encodeURIComponent(activeFolder)}`:''}`;
    const [result,normal]=await Promise.all([api<{notes:Note[]}>(url(filter),'GET',undefined,signal),trashReadOnly?api<{notes:Note[]}>(url('all'),'GET',undefined,signal):Promise.resolve({notes:[] as Note[]})]);
    if(mounted.current&&!signal?.aborted&&version===listVersion.current)setNotes([...new Map([...normal.notes,...result.notes].map(note=>[note.id,note])).values()]);
  },[filter,query,folderId,trashFolderId,trashReadOnly]);
  const refreshStatus = useCallback(async(signal?:AbortSignal)=>{
    const result=await api<Status>('/api/status','GET',undefined,signal);
    if(mounted.current&&!signal?.aborted)setStatus(result);
    return result;
  },[]);
  const refreshFolders = useCallback(async(signal?:AbortSignal)=>{
    const result=await api<{folders:NoteFolder[];trash?:NoteFolder[];initialized:boolean;counts?:{all:number;trash:number}}>('/api/folders','GET',undefined,signal);
    if(mounted.current&&!signal?.aborted){setFolders(result.folders);setTrashedFolders(result.trash||[]);setFoldersInitialized(result.initialized);setLibraryCounts(result.counts??null);}
  },[]);
  useEffect(()=>{
    if(!authorized)return;
    const controller=new AbortController();let timer:ReturnType<typeof setTimeout>;
    setListLoading(true);
    const poll=async()=>{
      const id=selectedRef.current;
      const tasks:Promise<unknown>[]=[refreshStatus(controller.signal),refreshList(controller.signal),refreshFolders(controller.signal)];
      if(id&&focusRef.current&&detailRef.current?.id===id&&!savingRef.current.has(id))tasks.push(readDetail(id,controller.signal));
      const results=await Promise.allSettled(tasks);
      if(controller.signal.aborted)return;
      setListLoading(false);
      const failed=results.find(r=>r.status==='rejected');
      setPollError(failed?.status==='rejected'?message(failed.reason):'');
      timer=setTimeout(()=>void poll(),4000);
    };
    timer=setTimeout(()=>void poll(),160);
    return()=>{controller.abort();clearTimeout(timer);};
  },[authorized,refreshList,refreshStatus,refreshFolders,readDetail]);
  useEffect(()=>{setSelectedCards(new Set());setSelectionMode(false);},[filter,folderId,trashFolderId,query]);
  useEffect(()=>{
    if(!selected){setDetail(null);return;}
    const controller=new AbortController();
    setDetail(null); setDetailLoading(true);
    readDetail(selected,controller.signal).catch(e=>{if(!controller.signal.aborted)setError(message(e));}).finally(()=>{if(!controller.signal.aborted)setDetailLoading(false);});
    return()=>controller.abort();
  },[selected,readDetail]);
  useEffect(()=>{
    if(!settings&&!moveTargets.length&&!folderDialog)return;
    const previous=document.activeElement as HTMLElement|null;
    const dialog=modalRef.current;
    (dialog?.querySelector<HTMLElement>('[data-autofocus]')??dialog?.querySelector<HTMLElement>('input,button'))?.focus();
    const trap=(event:KeyboardEvent)=>{
      if(event.key==='Escape'){setSettings(false);setMoveTargets([]);setFolderDialog(null);return;}
      if(event.key!=='Tab'||!dialog)return;
      const buttons=[...dialog.querySelectorAll<HTMLElement>('button:not(:disabled),select:not(:disabled),input:not(:disabled),[role="listbox"]')];
      const first=buttons[0],last=buttons[buttons.length-1];
      if(event.shiftKey&&document.activeElement===first){event.preventDefault();last?.focus();}
      if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first?.focus();}
    };
    document.addEventListener('keydown',trap);
    return()=>{document.removeEventListener('keydown',trap);previous?.focus();};
  },[settings,moveTargets.length,folderDialog]);
  const dirtyCount = new Set([...Object.keys(drafts).filter(id=>drafts[id].dirty),...ink.dirtyIds,...(ink.capturing&&selected?[selected]:[])]).size;
  useEffect(()=>{const prevent=(e:BeforeUnloadEvent)=>{if(dirtyCount){e.preventDefault();e.returnValue='';}};window.addEventListener('beforeunload',prevent);return()=>window.removeEventListener('beforeunload',prevent);},[dirtyCount]);
  useEffect(()=>{if(!notice)return;const t=setTimeout(()=>setNotice(''),4500);return()=>clearTimeout(t);},[notice]);

  const save = useCallback(async()=>{
    if(inkDirty){if(!busy&&showingPage&&!readingMode&&!draftsRef.current[selected||'']?.dirty)await ink.save();return;}
    if(ink.eraseDirty){if(!busy&&showingPage&&!readingMode)await ink.saveErase();return;}
    if(ink.moveDirty){if(!busy&&showingPage&&!readingMode)await ink.saveMove();return;}
    if(ink.anySaving)return;
    if(trashReadOnly||busy || !selected || !detail || (!detail.editable&&!detail.titleEditable&&!richAvailable) || detail.trashed || detail.syncState==='conflict')return;
    const id=selected, draft=draftsRef.current[id];
    if(!draft?.dirty || savingRef.current.has(id))return;
    if(draft.editor&&!richAvailable){setError('Yazdıklarınız korunuyor, ancak bu not şu anda kaydedilemiyor.');return;}
    savingRef.current.add(id); setSaving(id); setError('');
    try {
      const {note}=richAvailable&&draft.editor
        ?await api<{note:Note}>(notePath(id)+'/editor','PUT',{title:draft.title,model:draft.editor,expectedRevision:draft.revision})
        :detail.editable&&!bodyCapabilityBlocked
        ?await api<{note:Note}>(notePath(id),'PUT',{title:draft.title,body:draft.body,expectedRevision:draft.revision})
        :await api<{note:Note}>(notePath(id)+'/title','PATCH',{title:draft.title,expectedRevision:draft.revision});
      setDrafts(previous=>{const current=previous[id];return {...previous,[id]:{...current,revision:note.revision,dirty:current.title!==draft.title || current.body!==draft.body||JSON.stringify(current.editor)!==JSON.stringify(draft.editor),saveError:undefined}};});
      setDetail(previous=>previous?.id===id ? note : previous);
      setNotes(previous=>previous.map(n=>n.id===id?note:n));
      
      void refreshStatus().catch(()=>{});
    }catch(e){const text=e instanceof ApiError && e.status===409 ? 'Notun başka bir sürümü kaydedildi. Yazdıklarınız korunuyor; yeniden yüklemeden önce metninizi kopyalayın.' : message(e);setDrafts(previous=>({...previous,[id]:{...previous[id],saveError:text}}));setError(text);}
    finally{savingRef.current.delete(id);setSaving(null);}
  },[selected,detail,refreshStatus,busy,richAvailable,bodyCapabilityBlocked,inkDirty,ink,showingPage,readingMode,trashReadOnly]);
  const run = async(name:string, action:()=>Promise<void>)=>{if(busy)return;setBusy(name);setError('');try{await action();}catch(e){setError(message(e));}finally{setBusy(null);}};
  const create = useCallback(async()=>{
    if(busy)return;setBusy('new');setError('');
    // The note kind must not depend on whether the first status poll has answered yet.
    try{const rich=(status??await refreshStatus()).capabilities.richTextEditing;const {note}=rich?await api<{note:Note}>('/api/notes/editor','POST',{title:'',model:{schemaVersion:1,text:'',spans:[],paragraphs:[]}}):await api<{note:Note}>('/api/notes','POST',{title:'',body:''});setFilter('all');setFolderId(null);setQuery('');setNotes(previous=>[note,...previous]);setSelected(note.id);setFocusMode(true);}catch(e){setError(message(e));}finally{setBusy(null);}
  },[busy,status,refreshStatus]);
  const organizeReason=(note:Note|undefined,patch:OrganizePatch)=>{
    if(trashReadOnly&&(!note?.trashed||!('trashed' in patch)||patch.trashed!==false))return 'Bu klasörde yalnız çöp kutusundaki notlar geri yüklenebilir.';
    const capability='favorite' in patch?'cloudFavorites':'trashed' in patch?'cloudTrash':'cloudFolders';
    if(!status?.capabilities[capability])return 'Bu bulut işlemi henüz kullanılamıyor. İlk eşitlemenin tamamlanmasını bekleyin.';
    if(!note||note.source!=='cloud')return 'Önce notun buluta gönderilmesi gerekiyor.';
    if(!note.contentLoaded)return 'Bu işlem için notun içeriğini önce bağlantınız varken açın.';
    if(note.metadataSync?.state!=='synced')return 'Önce bu notun bekleyen bulut işlemi tamamlanmalı.';
    if(['pending','error','uncertain','conflict'].includes(note.syncState))return 'Önce notun eşitleme durumu çözülmeli.';
    if(ink.hasDraft(note.id))return 'Önce bu notun çizim taslağını kaydedin.';
    if(ink.anySaving||savingRef.current.has(note.id))return 'Not kaydediliyor.';
    if(draftsRef.current[note.id]?.dirty&&draftsRef.current[note.id].revision!==note.revision)return 'Buluttaki not değişti. Önce yazdıklarınızı koruyup sürüm farkını çözün.';
    if('trashed' in patch&&draftsRef.current[note.id]?.dirty)return 'Çöp kutusu işleminden önce değişikliklerinizi kaydedin.';
    return '';
  };
  const acceptMutation=(note:Note)=>{
    listVersion.current++;
    // The server returns queued state. Never infer remote success from HTTP 200.
    setDetail(previous=>previous?.id===note.id?note:previous);
    setNotes(previous=>previous.map(n=>n.id===note.id?note:n));
    setDrafts(previous=>previous[note.id]?{...previous,[note.id]:{...previous[note.id],revision:note.revision}}:previous);
  };
  const organize=async(patch:OrganizePatch)=>{
    const note=detail;if(!note)return;
    const reason=organizeReason(note,patch);if(reason){setError(reason);return;}
    await run('organize',async()=>{
      const result=await api<{note:Note}>(notePath(note.id),'PATCH',{...patch,expectedRevision:note.revision});
      acceptMutation(result.note);
      await Promise.allSettled([refreshList(),refreshStatus(),refreshFolders()]);
    });
  };
  const organizeSelection=(patch:OrganizePatch,ids=[...selectedCards])=>run('selection',async()=>{
    const failed:string[]=[];let accepted=0;
    for(const id of ids){
      const note=detail?.id===id?detail:notes.find(n=>n.id===id);
      if(organizeReason(note,patch)){failed.push(id);continue;}
      try{const result=await api<{note:Note}>(notePath(id),'PATCH',{...patch,expectedRevision:note!.revision});acceptMutation(result.note);accepted++;}
      catch{failed.push(id);}
    }
    setSelectedCards(new Set(failed));setSelectionMode(failed.length>0);
    await Promise.allSettled([refreshList(),refreshStatus(),refreshFolders()]);
    if(accepted)
    if(failed.length)setError(`${failed.length} not için işlem kuyruğa alınamadı. Bu notlar seçili kaldı; eşitleme durumlarını kontrol edin.`);
  });
  const openMove=(ids:string[])=>{setMoveTargets(ids);setMoveFolder(null);setNoteMenu(false);};
  const noteRows=(ids:string[])=>ids.map(id=>detail?.id===id?detail:notes.find(note=>note.id===id));
  // Why a folder (null: no folder) cannot receive these notes; '' when it can. Shared by the move dialog and drag-and-drop.
  const moveTargetReason=(ids:string[],target:string|null)=>{
    if(target!==null){const folder=folders.find(f=>f.id===target);if(!folder)return 'Klasör bulunamadı.';if(folder.system)return 'Sistem klasörlerine not taşınamaz.';if(folder.syncState!=='synced')return 'Klasörün eşitlemesi tamamlanmalı.';}
    const rows=noteRows(ids);
    if(rows.length&&rows.every(note=>(note?.folderId??null)===target))return ids.length>1?'Seçili notlar zaten burada.':'Not zaten burada.';
    return '';
  };
  // The one move path: the dialog and a drop both end here (same API, revision checks and notices).
  const moveTo=async(ids:string[],target:string|null)=>{if(target&&!folders.some(f=>f.id===target&&f.syncState==='synced')){setError('Hedef klasörün eşitlemesi tamamlanmalı.');return;}if(ids.length===1&&detail?.id===ids[0])await organize({folderId:target});else await organizeSelection({folderId:target},ids);};
  const moveNotes=async(choice:string|null=moveFolder)=>{if(choice===null||busy)return;const reason=moveTargetReason(moveTargets,choice||null);if(reason){setError(reason);return;}const ids=moveTargets;setMoveTargets([]);await moveTo(ids,choice||null);};
  const editFolder=(kind:FolderDialog['kind'],folder?:NoteFolder)=>{setFolderDialog({kind,folder});setFolderName(folder?.name||'');setFolderParent(folder?.parentId||'');setContextMenu(null);setError('');};
  const resolveFolder=(folder:NoteFolder)=>run('folder-resolve',async()=>{const result=await api<{folder:NoteFolder|null}>(`/api/folders/${encodeURIComponent(folder.id)}/resolve`,'POST',{strategy:'use_remote'});if(!result.folder&&folderId===folder.id)setFolderId(null);setContextMenu(null);setNotice('Buluttaki klasör durumu yeniden alındı.');await Promise.allSettled([refreshFolders(),refreshList(),refreshStatus()]);});
  const submitFolder=()=>run('folder',async()=>{
    if(!folderDialog)return;
    const {kind,folder}=folderDialog;
    if(['create','move'].includes(kind)&&folderParent&&!folders.some(f=>f.id===folderParent&&f.syncState==='synced'))throw new Error('Üst klasörün eşitlemesi tamamlanmalı.');
    const body=kind==='create'?{name:folderName,parentId:folderParent||null}:kind==='rename'?{name:folderName}:kind==='move'?{parentId:folderParent||null}:{trashed:kind==='trash'};
    await api(kind==='create'?'/api/folders':`/api/folders/${encodeURIComponent(folder!.id)}`,kind==='create'?'POST':'PATCH',body);
    // Reveal a folder created or moved under a collapsed parent.
    if(['create','move'].includes(kind)&&folderParent)setExpandedFolders(previous=>{const next=new Set(previous);let cursor:string|null=folderParent;while(cursor&&!next.has(cursor)){next.add(cursor);cursor=folders.find(f=>f.id===cursor)?.parentId??null;}return next;});
    setFolderDialog(null);
    if(kind==='restore'&&folder?.id===trashFolderId){chooseTrashFolder(trashParent(folder));await Promise.allSettled([refreshFolders(),refreshStatus()]);}
    else await Promise.allSettled([refreshFolders(),refreshList(),refreshStatus()]);
  });
  const sync = async()=>{if(busy)return;setBusy('sync');setSyncError('');try{await api('/api/sync','POST',{});setThumbnailRefresh(value=>value+1);await refreshStatus();await refreshList();}catch(e){setSyncError(message(e));}finally{setBusy(null);}};
  const login = async()=>{if(busy)return;setBusy('login');setAccountError('');try{const result=await api<{started:boolean}>('/api/auth/login','POST',{});await refreshStatus();setNotice(result.started===false?'Tarayıcıda devam eden giriş bekleniyor.':'Giriş için sistem tarayıcınız açıldı.');}catch(e){setAccountError(message(e));}finally{setBusy(null);}};
  const disconnect=async()=>{if(busy)return;setBusy('disconnect');setAccountError('');try{await api('/api/auth/disconnect','POST',{});await refreshStatus();setNotice('Hesap bağlantısı kesildi. Yerel notlarınız korundu.');}catch(e){setAccountError(message(e));}finally{setBusy(null);}};
  const resolve = (strategy:'keep_local_copy'|'use_remote')=>run('resolve',async()=>{
    if(!detail)return;
    const {note}=await api<{note:Note}>(notePath(detail.id)+'/resolve','POST',{strategy});
    setDrafts(previous=>({...previous,[note.id]:{title:note.title,body:note.body||'',revision:note.revision,dirty:false}}));setDetail(note);setSelected(note.id);await refreshList();await refreshStatus();
  });
  const copyNote = ()=>run('copy',async()=>{if(!detail)return;if(ink.hasDraft(detail.id)||ink.anySaving){setError('Önce çizim taslağınızı kaydedin.');return;}const {note}=await api<{note:Note}>('/api/notes','POST',{title:(draftsRef.current[detail.id]?.title||detail.title||untitledName(detail))+' — kopya',body:draftsRef.current[detail.id]?.body??detail.body??''});setFilter('all');setFolderId(null);setQuery('');setSelected(note.id);await refreshList();setNotice('Metin yeni bir nota kopyalandı.');});
  // Duplicate like Samsung's "Duplicate note": formatting survives through the
  // verified rich create path; plain notes use the plain create path.
  const duplicateNote = ()=>run('duplicate',async()=>{
    if(!detail)return;
    const title=(draftsRef.current[detail.id]?.title||detail.title||untitledName(detail))+' — kopya';
    const model=draftsRef.current[detail.id]?.editor??(editorInfo?.id===detail.id?editorInfo.projection.model:null);
    const {note}=richAvailable&&model?await api<{note:Note}>('/api/notes/editor','POST',{title,model}):await api<{note:Note}>('/api/notes','POST',{title,body:draftsRef.current[detail.id]?.body??detail.body??''});
    setFilter('all');setFolderId(null);setQuery('');setSelected(note.id);await refreshList();setNotice('Notun kopyası oluşturuldu.');
  });
  const openStyleMenu=()=>{if(styleMenu)setStyleMenu(null);else if(detail)setStyleMenu('loading');};
  const styleOpen=styleMenu!==null;
  useEffect(()=>{
    if(!styleOpen||!detail)return;
    const controller=new AbortController();const id=detail.id;
    void Promise.all([
      api<PageStyleInfo>(notePath(id)+'/page-style','GET',undefined,controller.signal),
      api<{templates:{id:string;name:string;pageCount:number;inNote?:boolean}[]}>(`/api/templates?note=${encodeURIComponent(id)}`,'GET',undefined,controller.signal).catch(()=>({templates:[]})),
    ]).then(([style,catalog])=>{if(!controller.signal.aborted)setStyleMenu(previous=>previous?{...style,templates:catalog.templates}:null);})
      .catch(e=>{if(!controller.signal.aborted){setStyleMenu(null);setError(message(e));}});
    return()=>controller.abort();
  },[styleOpen,detail?.id,detail?.revision,detail?.syncState,detail?.metadataSync?.state]);
  // Page settings → Background colour: every page, queued like other Cloud edits.
  // Insert → PDF: the file's pages are appended to the note, like Samsung's Insert PDF.
  const openSorter=()=>run('page-ops',async()=>{if(!detail)return;setNoteMenu(false);setSorter(await api<PageOpsInfo>(notePath(detail.id)+'/page-ops'));});
  const pageOp=(body:Record<string,unknown>)=>run('page-ops',async()=>{
    if(!detail)return;
    if(ink.hasDraft(detail.id)||ink.anySaving)throw new Error('Önce çizim taslağınızı kaydedin.');
    const result=await api<{note:Note}>(notePath(detail.id)+'/page-ops','POST',{...body,expectedRevision:detail.revision});
    acceptMutation(result.note);
    setSorter(await api<PageOpsInfo>(notePath(detail.id)+'/page-ops'));
    await Promise.allSettled([refreshList(),refreshStatus()]);
  });
  const applyTemplate=(templateId:string)=>run('template',async()=>{
    if(!detail)return;
    if(ink.hasDraft(detail.id)||ink.anySaving)throw new Error('Önce çizim taslağınızı kaydedin.');
    const result=await api<{note:Note}>(notePath(detail.id)+'/template','POST',{templateId,expectedRevision:detail.revision});
    acceptMutation(result.note);setStyleMenu(null);
    await Promise.allSettled([refreshList(),refreshStatus()]);
  });
  const insertPdf=(file:File)=>run('pdf',async()=>{
    if(!detail)return;
    if(ink.hasDraft(detail.id)||ink.anySaving)throw new Error('Önce çizim taslağınızı kaydedin.');
    const bytes=await file.arrayBuffer();
    const response=await request(`${notePath(detail.id)}/pdf?revision=${detail.revision}&name=${encodeURIComponent(file.name)}`,{method:'POST',body:bytes,headers:{'Content-Type':'application/pdf'}});
    const result=await response.json() as {note:Note};
    acceptMutation(result.note);
    await Promise.allSettled([refreshList(),refreshStatus()]);
  });
  const setPageColor=(argb:number)=>run('page-style',async()=>{
    if(!detail||!styleMenu||styleMenu==='loading')return;
    if(ink.hasDraft(detail.id)||ink.anySaving)throw new Error('Önce çizim taslağınızı kaydedin.');
    const result=await api<{note:Note}>(notePath(detail.id)+'/page-style','PUT',{backgroundArgb:argb,expectedRevision:detail.revision});
    acceptMutation(result.note);setStyleMenu(null);
    await Promise.allSettled([refreshList(),refreshStatus()]);
  });
  const shareText = ()=>{if(!detail)return;const text=[draftsRef.current[detail.id]?.title||detail.title,draftsRef.current[detail.id]?.body??detail.body??''].filter(Boolean).join('\n\n');void navigator.clipboard.writeText(text).then(()=>setNotice('Not metni panoya kopyalandı.'),()=>setError('Pano kullanılamadı.'));};
  const exportNote = ()=>run('export',async()=>{
    if(!detail)return;
    // Desktop app: the engine writes the file into Downloads (WebKit downloads go nowhere).
    const format=detail.editable||richAvailable||!pagePreview.pages.some(p=>p.available)?'txt':'pdf';
    const result=await api<{name:string;pages:number;skipped:number}>(notePath(detail.id)+'/export-file','POST',{format});
    setNotice(`Dosya hazır: ${result.name}${result.skipped?` (${result.skipped} sayfa hazırlanamadı)`:''}`);
  });
  const draft = selected ? drafts[selected] : undefined;
  const canEditTitle = !trashReadOnly&&!inkDirty&&!ink.capturing&&!ink.anySaving&&!ink.projection?.pendingStrokes.length&&!!detail?.titleEditable && !detail.trashed && detail.syncState!=='conflict' && !!draft;
  const inkBlocked=trashReadOnly||!!draft?.dirty||!!busy||!!saving||readingMode||!!detail?.trashed||detail?.syncState!=='synced';
  // Our own ink sync in flight does not stop drawing; any other pending edit does.
  const inkDrawBlocked=trashReadOnly||!!draft?.dirty||!!busy||!!saving||readingMode||!!detail?.trashed||!(detail?.syncState==='synced'||(detail?.syncState==='pending'&&inkPending));
  const inkCanDraw=showingPage&&canDrawInk(ink.tool==='highlighter'?{...ink,tool:'pen'}:ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked);
  // The rail asks whether a highlighter could start, whatever tool is active now.
  const inkHighlighterCanDraw=canDrawInk({...ink,tool:'highlighter'},pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked);
  const inkCanSave=showingPage&&canAppendInk(ink,pagePreview.pageIndex,inkBaselineComplete,inkBlocked);
  // Samsung clients have no explicit save: ink drafts persist shortly after the
  // last line, once the previous batch has synced.
  // A failed save is not retried automatically; editing or rebasing the draft clears the error.
  useEffect(()=>{if(!inkDirty||!inkCanSave||ink.capturing||ink.error)return;const timer=setTimeout(()=>void save(),1500);return()=>clearTimeout(timer);},[inkDirty,inkCanSave,ink.capturing,ink.draft,ink.error]);
  useEffect(()=>{if(inkDirty||!ink.canSaveErase||ink.capturing||ink.error||detail?.syncState!=='synced')return;const timer=setTimeout(()=>void ink.saveErase(),1500);return()=>clearTimeout(timer);},[inkDirty,ink.canSaveErase,ink.capturing,ink.erasure,ink.error,detail?.syncState]);
  const inkPage=ink.projection?.pages.find(page=>page.index===pagePreview.pageIndex);
  const canRichEdit=richAvailable&&!trashReadOnly&&!detail?.trashed&&detail?.syncState!=='conflict';
  // Windows Samsung Notes shortcuts (docs/WINDOWS_SHORTCUTS.md); a handler is set only while its action is possible.
  const menusOpen=noteMenu||viewMenu||shareMenu||insertMenu||!!styleMenu;
  // One rule for every dropdown: a press outside its anchor (toggle + panel) or Escape closes it.
  useEffect(()=>{
    if(!menusOpen)return;
    const closeAll=()=>{setNoteMenu(false);setViewMenu(false);setShareMenu(false);setInsertMenu(false);setStyleMenu(null);};
    const press=(event:PointerEvent)=>{const anchor=event.target instanceof Element?event.target.closest('.dropdown-anchor'):null;if(!anchor?.querySelector('.dropdown'))closeAll();};
    const key=(event:KeyboardEvent)=>{if(event.key==='Escape')closeAll();};
    document.addEventListener('pointerdown',press,true);document.addEventListener('keydown',key);
    return()=>{document.removeEventListener('pointerdown',press,true);document.removeEventListener('keydown',key);};
  },[menusOpen]);
  const editorOpen=focusMode&&!!detail;
  const toggleFullScreen=()=>{if(document.fullscreenElement)void document.exitFullscreen();else void document.documentElement.requestFullscreen().catch(()=>setError('Tam ekran bu pencerede desteklenmiyor.'));};
  const format=(command:FormatCommand)=>canRichEdit&&!readingMode&&richEditor&&!menusOpen?()=>richEditor.view.hasFocus()?void runFormat(richEditor,command):false:null;
  const shortcutHandlers:ShortcutHandlers=menusOpen?{pageSettings:styleMenu?()=>void openStyleMenu():null}:{
    newNote:create,saveNote:save,searchNotes:()=>{setFocusMode(false);requestAnimationFrame(()=>searchRef.current?.focus());},
    zoomIn:editorOpen?()=>setZoom(value=>Math.min(150,value+10)):null,
    zoomOut:editorOpen?()=>setZoom(value=>Math.max(50,value-10)):null,
    readingMode:editorOpen?()=>setReadingMode(value=>!value):null,
    fullScreen:editorOpen?toggleFullScreen:null,
    penSettings:editorOpen&&!readingMode?()=>setToolsOpen(!toolsOpen):null,
    pageSettings:editorOpen&&!readingMode&&!trashReadOnly?()=>void openStyleMenu():null,
    printNote:editorOpen?()=>window.print():null,
    bold:format('bold'),italic:format('italic'),underline:format('underline'),strikethrough:format('strikethrough'),
    alignLeft:format('alignLeft'),alignCenter:format('alignCenter'),alignRight:format('alignRight'),
    numberedList:format('numberedList'),bulletList:format('bulletList'),checklist:format('checklist'),
    fontSmaller:format('fontSmaller'),fontLarger:format('fontLarger'),indent:format('indent'),outdent:format('outdent'),
  };
  const shortcuts=useShortcuts({authorized,enabled:authorized&&!settings&&!contextMenu&&!folderDialog&&!moveTargets.length&&!sorter,actions:shortcutHandlers});
  // Fixed Windows keys: Enter leaves reading mode; Ctrl + mouse wheel zooms the page.
  useEffect(()=>{
    if(!focusMode)return;
    const key=(event:KeyboardEvent)=>{
      if(!readingMode||event.key!=='Enter'||event.ctrlKey||event.altKey||event.shiftKey||event.metaKey||event.defaultPrevented)return;
      const target=event.target as HTMLElement|null;
      if(target?.closest('input,textarea,select,button,a,[contenteditable="true"],[role="menu"],[role="dialog"]'))return;
      event.preventDefault();setReadingMode(false);
    };
    const wheel=(event:WheelEvent)=>{
      if(!event.ctrlKey||!(event.target instanceof Element)||!event.target.closest('.canvas-scroll'))return;
      event.preventDefault();if(event.deltaY)setZoom(value=>event.deltaY<0?Math.min(150,value+10):Math.max(50,value-10));
    };
    window.addEventListener('keydown',key);window.addEventListener('wheel',wheel,{passive:false});
    return()=>{window.removeEventListener('keydown',key);window.removeEventListener('wheel',wheel);};
  },[focusMode,readingMode]);
  const canEdit = !trashReadOnly&&!!detail?.editable && !bodyCapabilityBlocked && !detail.trashed && detail.syncState!=='conflict' && !!draft;
  const findInDocument=(value:string)=>{
    if(!value)return;const at=(draft?.body||'').toLocaleLowerCase('tr').indexOf(value.toLocaleLowerCase('tr'));if(at<0)return;
    if(richAvailable&&richEditor){const position=(target:number)=>{let consumed=0,result=1;richEditor.state.doc.forEach((node,offset)=>{if(target>=consumed&&target<=consumed+node.content.size)result=offset+1+target-consumed;consumed+=node.content.size+1;});return result;};richEditor.commands.setTextSelection({from:position(at),to:position(at+value.length)});}
    else bodyRef.current?.setSelectionRange(at,at+value.length);
  };
  // An event that leaves the text as it was (IME commit, programmatic focus) is not an edit.
  const updateDraft = (patch:Partial<Pick<Draft,'title'|'body'>>)=>{if(selected)setDrafts(previous=>{const current=previous[selected];if(current&&Object.entries(patch).every(([key,value])=>current[key as 'title'|'body']===value))return previous;return {...previous,[selected]:{...current,...patch,dirty:true,saveError:undefined}};});};
  const currentNav=navItems.find(n=>n.id===filter)!;
  const currentFolder=folders.find(f=>f.id===folderId);
  const allTrashFolders=[...trashedFolders,...folders];
  const inTrashedFolder=(id:string|null)=>{const seen=new Set<string>();let folder=allTrashFolders.find(item=>item.id===id);while(folder&&!seen.has(folder.id)){if(folder.state==='trashed')return true;seen.add(folder.id);folder=allTrashFolders.find(item=>item.id===folder?.parentId);}return false;};
  const orderedNotes=notes.filter(note=>filter!=='trash'||!!trashFolderId||!!query||!inTrashedFolder(note.folderId)).sort((a,b)=>(sortAscending?1:-1)*((a.modifiedAt??0)-(b.modifiedAt??0)));
  const currentTrashFolder=filter==='trash'?allTrashFolders.find(f=>f.id===trashFolderId):undefined;
  const trashParent=(folder:NoteFolder|undefined)=>{const parent=allTrashFolders.find(f=>f.id===folder?.parentId);if(!parent)return null;const seen=new Set<string>();let cursor:NoteFolder|undefined=parent;while(cursor&&!seen.has(cursor.id)){if(cursor.state==='trashed')return parent.id;seen.add(cursor.id);cursor=allTrashFolders.find(f=>f.id===cursor?.parentId);}return null;};
  const visibleTrashFolders=filter==='trash'&&!query?(trashFolderId?allTrashFolders.filter(f=>f.parentId===trashFolderId):trashedFolders.filter(f=>!trashParent(f))):[];
  const chooseTrashFolder=(id:string|null)=>{listVersion.current++;setNotes([]);setListLoading(true);setTrashFolderId(id);setContextMenu(null);};
  useEffect(()=>setContextMenu(null),[focusMode,filter,folderId,trashFolderId,query,galleryMode]);
  const chooseFilter=(value:Filter)=>{setFilter(value);setFolderId(null);setTrashFolderId(null);setSidebar(false);};
  const selectedRows=[...selectedCards].map(id=>notes.find(n=>n.id===id));
  const selectionReason=(patch:OrganizePatch)=>!selectedRows.length?'Önce bir not seçin.':selectedRows.map(n=>organizeReason(n,patch)).find(Boolean)||'';
  // Samsung folder tree: API order within siblings, collapsed by default.
  // A count is direct notes plus direct subfolders; empty device folders hide.
  const folderChildren=(parent:string|null)=>folders.filter(f=>f.parentId===parent&&!(f.system&&f.noteCount===0&&!folders.some(c=>c.parentId===f.id)));
  const folderCount=(folder:NoteFolder)=>folder.noteCount+folderChildren(folder.id).length;
  const folderRows:{folder:NoteFolder;depth:number}[]=[];
  const walkFolders=(parent:string|null,depth:number)=>{for(const folder of folderChildren(parent)){folderRows.push({folder,depth});if(expandedFolders.has(folder.id)&&depth<8)walkFolders(folder.id,depth+1);}};
  walkFolders(null,0);
  const toggleFolder=(id:string)=>setExpandedFolders(previous=>{const next=new Set(previous);if(next.has(id))next.delete(id);else next.add(id);return next;});
  const chooseFolder=(folder:NoteFolder)=>{setFolderId(folder.id);setFilter('all');setSidebar(false);setContextMenu(null);setFoldersOpen(true);setExpandedFolders(previous=>{const next=new Set(previous),seen=new Set<string>();let parent=folder.parentId;while(parent&&!seen.has(parent)){seen.add(parent);next.add(parent);parent=folders.find(item=>item.id===parent)?.parentId??null;}return next;});};
  const visibleChildFolders=filter==='all'&&folderId&&!query?folderChildren(folderId):[];
  // The whole tree regardless of the sidebar's expanded state, for folder pickers.
  const allFolderRows:{folder:NoteFolder;depth:number}[]=[];
  const walkAllFolders=(parent:string|null,depth:number,seen:Set<string>)=>{for(const folder of folderChildren(parent)){if(seen.has(folder.id))continue;seen.add(folder.id);allFolderRows.push({folder,depth});if(depth<8)walkAllFolders(folder.id,depth+1,seen);}};
  walkAllFolders(null,0,new Set());
  const folderLabel=(folder:NoteFolder)=>folder.name.replace(/\u200b/g,'');
  const moveOptions=(ids:string[]):FolderOption[]=>{
    const rows=noteRows(ids),first=rows[0]?.folderId??null;
    const shared=rows.length&&rows.every(note=>(note?.folderId??null)===first)?first:undefined;
    return [{id:'',label:'Klasörsüz',depth:0,icon:'none',current:shared===null,reason:moveTargetReason(ids,null)||undefined},
      ...allFolderRows.map(({folder,depth})=>({id:folder.id,label:folderLabel(folder),depth,count:folder.syncState==='synced'?folderCount(folder):undefined,current:shared===folder.id,reason:moveTargetReason(ids,folder.id)||undefined}))];
  };
  const parentOptions=(dialog:FolderDialog):FolderOption[]=>[{id:'',label:'Klasörler',depth:0,current:dialog.kind==='move'&&!dialog.folder?.parentId},
    ...allFolderRows.filter(({folder})=>!dialog.folder||!folderDescendant(folder,dialog.folder.id)).map(({folder,depth})=>({id:folder.id,label:folderLabel(folder),depth,current:dialog.kind==='move'&&dialog.folder?.parentId===folder.id,reason:folder.syncState!=='synced'?'Klasörün eşitlemesi tamamlanmalı.':undefined}))];
  // Press-and-hold a card, drop it on a sidebar folder: the same move as the dialog.
  const noteDrag=useNoteDrag({
    canDrag:id=>filter!=='trash'&&!!notes.find(note=>note.id===id&&!note.trashed),
    idsFor:id=>selectionMode&&selectedCards.has(id)?[...selectedCards]:[id],
    titleFor:ids=>{if(ids.length>1)return `${ids.length} not`;const note=noteRows(ids)[0];return note?(draftsRef.current[note.id]?.title||note.title||untitledName(note)):'Not';},
    noteReason:ids=>busy?'Başka bir işlem sürüyor.':noteRows(ids).map(note=>organizeReason(note,{folderId:null})).find(Boolean)||'',
    reason:(ids,target)=>(busy?'Başka bir işlem sürüyor.':noteRows(ids).map(note=>organizeReason(note,{folderId:target})).find(Boolean))||moveTargetReason(ids,target),
    targetLabel:target=>{if(target===null)return 'Klasörden çıkar (Klasörsüz)';const folder=folders.find(f=>f.id===target);return `${folder?folderLabel(folder):'Klasör'} klasörüne taşı`;},
    onDrop:(ids,target)=>void moveTo(ids,target),
    onExpand:id=>setExpandedFolders(previous=>previous.has(id)?previous:new Set(previous).add(id)),
  });
  const offlineView=!!status&&(status.sync.state==='offline'||(!status.connected&&foldersInitialized));
  const metadataState=detail?.metadataSync?.state;
  const metadataNeedsResolution=['error','uncertain','conflict'].includes(metadataState||'');
  const folderReady=(folder:NoteFolder)=>!!status?.capabilities.cloudFolders&&folder.syncState==='synced'&&!busy;
  const folderDescendant=(candidate:NoteFolder,ancestor:string)=>{let id:string|null=candidate.id;const seen=new Set<string>();while(id&&!seen.has(id)){if(id===ancestor)return true;seen.add(id);id=folders.find(f=>f.id===id)?.parentId??null;}return false;};
  const folderDialogTitle=folderDialog?({create:'Klasör oluştur',rename:'Klasörü yeniden adlandır',move:'Klasörü taşı',trash:'Klasörü çöp kutusuna taşı',restore:'Klasörü geri yükle'}[folderDialog.kind]):'';

  const contextEvents=(kind:ContextTarget['kind'],id:string)=>({
    'aria-haspopup':'menu' as const,
    onContextMenu:(event:MouseEvent<HTMLButtonElement>)=>{event.preventDefault();openContext(kind,id,event.currentTarget,event.clientX,event.clientY);},
    onKeyDown:(event:ReactKeyboardEvent<HTMLButtonElement>)=>{if(event.key==='ContextMenu'||(event.shiftKey&&event.key==='F10')){event.preventDefault();const rect=event.currentTarget.getBoundingClientRect();openContext(kind,id,event.currentTarget,rect.left+8,rect.bottom);}},
  });
  const openContext=(kind:ContextTarget['kind'],id:string,trigger:HTMLElement,x:number,y:number)=>{const rect=trigger.getBoundingClientRect();setNoteMenu(false);setViewMenu(false);setContextMenu({kind,id,ids:kind==='note'&&selectionMode&&selectedCards.has(id)?[...selectedCards]:[id],trigger,x:Number.isFinite(x)?x:rect.left+8,y:Number.isFinite(y)?y:rect.bottom});};
  // Card menu actions work on a closed note: read it first, never from a stale list preview.
  const cardText=async(note:Note)=>{const {note:full}=await api<{note:Note}>(notePath(note.id));return [draftsRef.current[note.id]?.title||full.title,draftsRef.current[note.id]?.body??full.body??''].filter(Boolean).join('\n\n');};
  const shareCard=(note:Note)=>run('share',async()=>{const text=await cardText(note);if(!text.trim())throw new Error('Bu notta paylaşılabilecek metin yok.');await navigator.clipboard.writeText(text).then(()=>setNotice('Not metni panoya kopyalandı.'),()=>{throw new Error('Pano kullanılamadı.');});});
  const exportCard=(note:Note)=>run('export',async()=>{
    let result:{name:string;skipped:number};
    try{result=await api(notePath(note.id)+'/export-file','POST',{format:note.editable?'txt':'pdf'});}
    catch(e){if(note.editable||!(e instanceof ApiError)||e.status!==422)throw e;result=await api(notePath(note.id)+'/export-file','POST',{format:'txt'});}
    setNotice(`Dosya hazır: ${result.name}${result.skipped?` (${result.skipped} sayfa hazırlanamadı)`:''}`);
  });
  const duplicateCard=(note:Note)=>run('duplicate',async()=>{
    const {note:full}=await api<{note:Note}>(notePath(note.id));
    const projection=await api<EditorProjection>(notePath(note.id)+'/editor').catch(()=>null);
    const title=(full.title||untitledName(full))+' — kopya';
    const model=projection?.schemaVersion===1&&projection.revision===full.revision&&projection.capabilities.textEdit&&projection.capabilities.richStyle?projection.model:null;
    if(!model&&!full.editable)throw new Error('Bu not henüz çoğaltılamıyor.');
    if(model)assertEditorModel(model);
    // A text-anchored picture cannot be created in a new note (its object and file stay with the original).
    if(model?.objects?.length)throw new Error('Metnine resim yerleşmiş notlar henüz çoğaltılamıyor.');
    const {note:copy}=model?await api<{note:Note}>('/api/notes/editor','POST',{title,model}):await api<{note:Note}>('/api/notes','POST',{title,body:full.body??''});
    setNotes(previous=>[copy,...previous]);await refreshList();setNotice('Notun kopyası oluşturuldu.');
  });
  const contextActions:ContextAction[]=[];
  if(contextMenu?.kind==='folder'){
    const folder=folders.find(item=>item.id===contextMenu.id);
    if(folder){
      if(['error','uncertain','conflict'].includes(folder.syncState))contextActions.push({label:'Buluttaki durumu kullan',disabled:!!busy,run:()=>void resolveFolder(folder)});
      contextActions.push({label:'Yeniden adlandır',disabled:!folderReady(folder),run:()=>editFolder('rename',folder)},
        {label:'Klasörü taşı',disabled:!folderReady(folder),icon:<Folder size={15}/>,run:()=>editFolder('move',folder)},
        {label:'Çöp kutusuna taşı',disabled:!folderReady(folder)||folder.noteCount>0||folders.some(item=>item.parentId===folder.id),description:'Yalnızca boş klasörler çöp kutusuna taşınabilir',icon:<Trash2 size={15}/>,run:()=>editFolder('trash',folder)});
    }
  }else if(contextMenu?.kind==='trashFolder'){
    const folder=trashedFolders.find(item=>item.id===contextMenu.id);
    if(folder){
      if(['error','uncertain','conflict'].includes(folder.syncState))contextActions.push({label:'Buluttaki durumu kullan',disabled:!!busy,run:()=>void resolveFolder(folder)});
      else contextActions.push({label:'Geri yükle',disabled:!folderReady(folder),description:folderReady(folder)?'':'Klasörün eşitlemesi tamamlanmalı.',run:()=>editFolder('restore',folder)});
    }
  }else if(contextMenu?.kind==='note'&&notes.some(note=>note.id===contextMenu.id)){
    // Windows order: Move, Lock, Share, Delete, Save as file, Duplicate note, Add to favorites.
    // Lock and permanent delete do not exist here and are not shown.
    const ids=contextMenu.ids,rows=ids.map(id=>notes.find(note=>note.id===id)),single=ids.length===1?rows[0]:undefined;
    const favorite=rows.every(note=>note?.favorite),trashed=rows.every(note=>note?.trashed);
    const reason=(patch:OrganizePatch)=>rows.map(note=>organizeReason(note,patch)).find(Boolean)||'';
    if(trashReadOnly&&!trashed){}
    else if(trashed)contextActions.push({label:'Geri yükle',disabled:!!busy||!!reason({trashed:false}),description:reason({trashed:false}),icon:<Trash2 size={15}/>,run:()=>void organizeSelection({trashed:false},ids)});
    else{
      contextActions.push({label:'Taşı',disabled:!!busy||!!reason({folderId:null}),description:reason({folderId:null}),icon:<Folder size={15}/>,run:()=>openMove(ids)});
      if(single)contextActions.push({label:'Paylaş',disabled:!!busy,description:'Notun metnini panoya kopyalar',run:()=>void shareCard(single)});
      contextActions.push({label:'Sil',disabled:!!busy||!!reason({trashed:true}),description:reason({trashed:true})||'Samsung Cloud çöp kutusuna taşır; 30 gün sonra kalıcı olarak silinir',icon:<Trash2 size={15}/>,run:()=>void organizeSelection({trashed:true},ids)});
      if(single)contextActions.push({label:'Dosya olarak kaydet',disabled:!!busy,description:'İndirilenler klasörüne kaydeder',run:()=>void exportCard(single)},
        {label:'Notu çoğalt',disabled:!!busy||!single.editable||ink.hasDraft(single.id),description:!single.editable?'El yazısı ve çizim notları henüz çoğaltılamıyor.':ink.hasDraft(single.id)?'Önce çizim taslağınızı kaydedin.':'',run:()=>void duplicateCard(single)});
      contextActions.push({label:favorite?'Favorilerden çıkar':'Favorilere ekle',disabled:!!busy||!!reason({favorite:!favorite}),description:reason({favorite:!favorite}),icon:<Heart size={15}/>,run:()=>void organizeSelection({favorite:!favorite},ids)});
    }
  }

  if(!authorized)return <main className="launch-error"><div className="app-mark"><NotebookPen size={27}/></div><h1>Notlarınıza güvenli bir giriş</h1><p>Yerel uygulama oturumu bulunamadı. Notlar’ı masaüstü başlatıcısından yeniden açın.</p><p className="muted">Samsung hesabınızın parolası bu uygulamada istenmez.</p></main>;
  return <div className={`workspace windows-workspace ${focusMode?'show-editor':''} ${drawerCollapsed?'drawer-collapsed':''} ${busy||saving?'is-busy':''}`}>
    {sidebar&&<button className="sidebar-shade" aria-label="Menüyü kapat" onClick={()=>setSidebar(false)}/>}
    <aside className={`sidebar ${sidebar?'open':''}`} aria-label="Ana gezinme">
      <header className="drawer-header"><button className="icon-button" aria-label="Menüyü kapat" onClick={()=>{setDrawerCollapsed(true);setSidebar(false);}}><Menu size={21}/></button></header>
      <nav><button className={`nav-item ${filter==='all'&&!folderId?'active':''} ${noteDrag.isTarget(null)?'drop-target':''}`} data-drop-target="" onClick={()=>chooseFilter('all')}><WinIcon name="ic_drawer_all"/><span>Tüm notlar</span><small>{libraryCounts?.all??(filter==='all'&&!folderId&&!query?notes.length:'')}</small></button><button className={`nav-item ${filter==='trash'?'active':''}`} title="Samsung Cloud çöp kutusu" onClick={()=>chooseFilter('trash')}><WinIcon name="ic_drawer_delete"/><span>Çöp kutusu</span><small>{libraryCounts?.trash??(filter==='trash'?notes.length:'')}</small></button><div className="drawer-separator"/><div className="folder-section-heading"><button className="nav-item" onClick={()=>setFoldersOpen(!foldersOpen)} aria-expanded={foldersOpen}><WinIcon name="ic_drawer_folder"/><span>Klasörler</span><small>{folderChildren(null).reduce((sum,folder)=>sum+folder.noteCount,0)||''}</small></button><button className="icon-button" aria-label="Klasör oluştur" disabled={!!busy||!status?.capabilities.cloudFolders} onClick={()=>editFolder('create')}><Plus size={16}/></button></div>{foldersOpen&&<div className="folder-list">{folderRows.map(({folder,depth})=><div className="folder-entry" key={folder.id}>{folderChildren(folder.id).length?<button className="folder-twisty" style={{left:depth*16}} aria-label={expandedFolders.has(folder.id)?`${folder.name} klasörünü daralt`:`${folder.name} klasörünü genişlet`} aria-expanded={expandedFolders.has(folder.id)} onClick={()=>toggleFolder(folder.id)}><ChevronRight size={13} className={expandedFolders.has(folder.id)?'expanded':''}/></button>:null}<button className={`nav-item folder-item ${folderId===folder.id?'active':''} ${noteDrag.isTarget(folder.id)?'drop-target':''}`} data-drop-target={folder.id} data-drop-expand={folderChildren(folder.id).length&&!expandedFolders.has(folder.id)?'':undefined} style={{paddingLeft:28+depth*16}} {...contextEvents('folder',folder.id)} onClick={()=>chooseFolder(folder)} title={folder.error||folder.name}><WinIcon name="ic_drawer_folder"/><span>{folder.name.replace(/\u200b/g,'')}</span>{folder.syncState!=='synced'?<span className={`state-dot ${folder.syncState}`} title={syncLabels[folder.syncState]}/>:<small>{folderCount(folder)}</small>}</button></div>)}{!folders.length&&foldersInitialized&&<p className="folder-hint">Henüz klasör yok.</p>}</div>}</nav>
      <div className="drawer-bottom"><div className="drawer-separator"/><button className="nav-item" onClick={()=>setSettings(true)}><WinIcon name="ic_drawer_setting"/><span>Ayarlar</span>{status?.connected?<Cloud size={15}/>:<CloudOff size={15}/>}</button></div>
    </aside>
    <section className="note-list" aria-label="Not listesi">
      <header className="list-header"><div className="list-title">{drawerCollapsed&&<button className="icon-button" aria-label="Menüyü aç" onClick={()=>setDrawerCollapsed(false)}><Menu size={20}/></button>}<button className="icon-button mobile-only" aria-label="Menüyü aç" onClick={()=>setSidebar(true)}><Menu size={20}/></button>{currentTrashFolder&&<button className="icon-button" aria-label="Çöp kutusuna dön" title="Geri" onClick={()=>chooseTrashFolder(trashParent(currentTrashFolder))}><ArrowLeft size={18}/></button>}<h1>{currentTrashFolder?.name||currentFolder?.name||currentNav.label}</h1>{currentTrashFolder?.state==='trashed'&&<button className="small-button" disabled={!folderReady(currentTrashFolder)} onClick={()=>editFolder('restore',currentTrashFolder)}>Klasörü geri yükle</button>}</div><div className="list-commandbar">{!trashReadOnly&&<><button className="command-button" aria-label="Yeni not" onClick={()=>void create()} disabled={!!busy}><SquarePen size={17}/><span>Not oluştur</span></button><button className="command-button optional-command" aria-label="PDF içe aktar" disabled title="PDF içe aktarma henüz desteklenmiyor"><WinIcon name="ac_ic_main_pdf"/><span>PDF içe aktar</span></button><button className={`command-button ${selectionMode?'active':''}`} onClick={()=>{setSelectionMode(!selectionMode);setSelectedCards(new Set());}}><CircleCheck size={17}/><span>{selectionMode?'Bitti':'Düzenle'}</span></button></>}<div className="dropdown-anchor"><button className="command-button" onClick={()=>setViewMenu(!viewMenu)} aria-expanded={viewMenu}><Grid3X3 size={17}/><span>Görünüm</span></button>{viewMenu&&<div className="dropdown view-menu"><button onClick={()=>{setGalleryMode('grid');setViewMenu(false);}}><Grid3X3 size={15}/>Izgara görünümü</button><button onClick={()=>{setGalleryMode('list');setViewMenu(false);}}><List size={15}/>Liste görünümü</button><hr/>{navItems.filter(n=>n.id!=='trash').map(({id,label,icon:Icon})=><button key={id} onClick={()=>{chooseFilter(id);setViewMenu(false);}}><Icon size={15}/>{label}</button>)}</div>}</div><label className="search"><input ref={searchRef} value={query} onChange={e=>setQuery(e.target.value)} placeholder="Ara" aria-label="Notlarında ara"/>{query?<button className="icon-button" onClick={()=>setQuery('')} aria-label="Aramayı temizle"><X size={16}/></button>:<Search size={17}/>}</label></div></header>
      <div className="list-order"><span className="library-status"/><div><button className="sort-button" onClick={()=>setSortAscending(!sortAscending)}><WinIcon name="sort_by" size={18}/>Değiştirilme tarihi</button><span className="sort-separator"/><button className="icon-button" aria-label="Sıralama yönünü değiştir" onClick={()=>setSortAscending(!sortAscending)}>{sortAscending?<ArrowUp size={18}/>:<ArrowDown size={18}/>}</button></div></div>
      {selectionMode&&<div className="selection-bar"><span>{selectedCards.size} not seçildi</span><button onClick={()=>setSelectedCards(new Set(orderedNotes.map(n=>n.id)))}>Tümünü seç</button><button disabled={!!busy||!!selectionReason({favorite:true})} title={selectionReason({favorite:true})||'Samsung Cloud favorilerini güncelle'} onClick={()=>void organizeSelection({favorite:true})}><WinIcon name="ic_drawer_favorite" size={17}/>Favorilere ekle</button><button disabled={!!busy||!!selectionReason({trashed:filter!=='trash'})} title={selectionReason({trashed:filter!=='trash'})||'Samsung Cloud çöp kutusunu güncelle'} onClick={()=>void organizeSelection({trashed:filter!=='trash'})}><WinIcon name="ic_drawer_delete" size={17}/>{filter==='trash'?'Geri yükle':'Çöp kutusuna taşı'}</button><button disabled={!!busy||!!selectionReason({folderId:null})} title={selectionReason({folderId:null})||'Samsung Cloud klasörüne taşı'} onClick={()=>openMove([...selectedCards])}><Folder size={17}/>Klasöre taşı</button></div>}
      {pollError&&<div className="alert warning" role="status"><CloudOff size={17}/><span>{pollError}</span></div>}
      
      {error&&!focusMode&&<div className="alert error" role="alert"><ShieldAlert size={17}/><span>{error}</span><button className="icon-button" aria-label="Uyarıyı kapat" onClick={()=>setError('')}><X size={15}/></button></div>}
      {filter==='trash'&&<p className="trash-subtitle">Öğelerde, kalıcı olarak silinmelerine kalan gün sayısı gösterilir.</p>}
      {visibleTrashFolders.length>0&&<div className="trash-folders" aria-label="Çöp kutusundaki klasörler">{visibleTrashFolders.map(folder=><button className="trash-folder-card" key={folder.id} {...(folder.state==='trashed'?contextEvents('trashFolder',folder.id):{})} onClick={()=>chooseTrashFolder(folder.id)} title={folder.error||folder.name}><span className="trash-folder-tab" aria-hidden="true"/><span className="trash-folder-body"><WinIcon name="ic_drawer_folder" size={18}/><strong>{folder.name.replace(/\u200b/g,'')}</strong>{daysLeft(folder.trashedAt)!==null?<small>{daysLeft(folder.trashedAt)} gün</small>:null}</span></button>)}</div>}
      <div className={`note-rows gallery-${galleryMode}`}>
      {visibleChildFolders.map(folder=><button key={folder.id} className={`trash-folder-card live-folder-card ${noteDrag.isTarget(folder.id)?'drop-target':''}`} data-drop-target={folder.id} aria-label={`${folderLabel(folder)}, ${folderCount(folder)} öğe`} title={folder.error||folderLabel(folder)} {...contextEvents('folder',folder.id)} onClick={()=>chooseFolder(folder)}><span className="trash-folder-tab" aria-hidden="true"/><span className="trash-folder-body"><WinIcon name="ic_drawer_folder" size={18}/><strong>{folderLabel(folder)}</strong><small>{folderCount(folder)} öğe</small></span></button>)}

      {listLoading&&!notes.length?<div className="list-empty"><LoaderCircle className="spin" size={23}/></div>:!orderedNotes.length&&!visibleChildFolders.length&&!visibleTrashFolders.length?<div className="list-empty"><FileText size={34}/><h3>{query?'Eşleşen not yok':filter==='trash'?(trashedFolders.length?'Çöp kutusunda not yok':'Çöp kutusu boş'):filter==='favorites'?'Henüz favori yok':filter==='conflicts'?'Çakışma yok':'Henüz not yok'}</h3><p>{query?'Başka bir kelimeyle arayın.':'Yeni bir not oluşturun veya Ayarlar’dan hesabınızı bağlayın.'}</p>{filter==='all'&&!query&&<button className="small-button" onClick={()=>void create()}>Yeni not</button>}</div>:orderedNotes.map(note=><button className={`note-row ${selectedCards.has(note.id)?'card-selected':''} ${noteDrag.isSource(note.id)?'drag-source':''}`} key={note.id} {...(!trashReadOnly||note.trashed?contextEvents('note',note.id):{})} {...(!trashReadOnly?noteDrag.bind(note.id):{})} onClick={()=>{if(noteDrag.consumeClick(note.id))return;if(selectionMode){setSelectedCards(previous=>{const next=new Set(previous);if(next.has(note.id))next.delete(note.id);else next.add(note.id);return next;});}else{setSelected(note.id);setFocusMode(true);}}}><NoteThumbnail note={note} text={drafts[note.id]?.dirty?drafts[note.id].body:note.preview} dirty={!!drafts[note.id]?.dirty} enabled={!focusMode} refresh={thumbnailRefresh} selectionMode={selectionMode} selected={selectedCards.has(note.id)} band={note.trashed&&daysLeft(note.trashedAt)!==null?`${daysLeft(note.trashedAt)} gün`:undefined}/><div className="note-row-title"><strong>{drafts[note.id]?.title||note.title||untitledName(note)}</strong></div><div className="note-row-meta"><span>{date(note.modifiedAt)}</span></div></button>)}
      </div>
    </section>
    <main className={`editor-panel ${readingMode?'reading-mode':''}`}>
      <header className="editor-toolbar"><div className="editor-heading"><button className="icon-button back-button" aria-label="Not listesine dön" title="Geri" onClick={()=>{setFocusMode(false);setNoteMenu(false);setShareMenu(false);setInsertMenu(false);setStyleMenu(null);}}><WinIcon name="back" size={18}/></button><input className="document-title" aria-label="Not başlığı" value={draft?.title??detail?.title??''} onChange={e=>updateDraft({title:e.target.value})} placeholder="Başlıksız not" readOnly={!canEditTitle||readingMode}/></div>{detail&&!trashReadOnly&&<div className="editor-actions"><button className="command-button optional-command" onClick={()=>{if(document.fullscreenElement)void document.exitFullscreen();else void document.documentElement.requestFullscreen().catch(()=>setError('Tam ekran bu pencerede desteklenmiyor.'));}}><Maximize2 size={15}/><span>Tam ekran</span></button><button className={`command-button optional-command ${readingMode?'active':''}`} onClick={()=>setReadingMode(!readingMode)}><BookOpen size={15}/><span>Okuma modu</span></button><button className="command-button optional-command" aria-label="Sayfalar" disabled={!canSelectPage} title={canSelectPage?'Sayfaya git':'Sayfa yok'} onClick={()=>{pageSelectorRef.current?.scrollIntoView({block:'nearest'});pageSelectorRef.current?.focus();pageSelectorRef.current?.click();}}><PanelTop size={15}/><span>Sayfalar</span></button><div className="dropdown-anchor"><button className="command-button optional-command" aria-label="Sayfa ayarları" aria-expanded={!!styleMenu} title="Sayfa arka plan rengi" onClick={()=>void openStyleMenu()}><FilePenLine size={15}/><span>Sayfa ayarları</span></button>{styleMenu&&<div className="dropdown style-menu" role="dialog" aria-label="Sayfa ayarları">{styleMenu==='loading'?<div className="style-menu-status"><LoaderCircle className="spin" size={15}/>Yükleniyor</div>:<><div className="style-menu-title">Arka plan rengi</div>{styleMenu.palette?.map((row,index)=><div className="swatch-row" key={index}>{row.map(color=>{const hex='#'+(color.argb&0xFFFFFF).toString(16).padStart(6,'0');const current=styleMenu.backgroundArgb===color.argb;return <button key={color.argb} className={`swatch ${current?'current':''} ${index===0&&color===row[0]?'default':''}`} style={{backgroundColor:hex}} aria-label={color.name} aria-pressed={current} title={color.name} disabled={!styleMenu.editable||detail.syncState!=='synced'||current||!!busy} onClick={()=>setPageColor(color.argb)}>{current&&<Check size={14}/>}</button>;})}</div>)}{!!styleMenu.templates?.length&&<><div className="style-menu-title template-title">Şablon ekle</div><div className="template-list">{styleMenu.templates.map(t=><button key={t.id} disabled={!styleMenu.editable||!!busy||t.inNote} title={t.inNote?'Bu şablon notta zaten var':`${t.pageCount} sayfa sona eklenir`} onClick={()=>applyTemplate(t.id)}><FileText size={15}/><span>{t.name}</span><small>{t.pageCount} sayfa</small></button>)}</div></>}</>}</div>}</div><div className="dropdown-anchor"><button className="command-button optional-command" aria-label="Ekle" aria-expanded={insertMenu} onClick={()=>setInsertMenu(!insertMenu)}><Plus size={17}/><span>Ekle</span></button>{insertMenu&&<div className="dropdown insert-menu"><button disabled={!!busy} onClick={()=>{setInsertMenu(false);pdfInput.current?.click();}}><FileText size={16}/>PDF</button></div>}<input ref={pdfInput} type="file" accept="application/pdf,.pdf" hidden aria-label="PDF dosyası seç" onChange={e=>{const file=e.target.files?.[0];e.target.value='';if(file)insertPdf(file);}}/></div><div className="dropdown-anchor"><button className="command-button optional-command" aria-label="Paylaş" aria-expanded={shareMenu} onClick={()=>setShareMenu(!shareMenu)}><Share2 size={15}/><span>Paylaş</span></button>{shareMenu&&<div className="dropdown share-menu"><button onClick={()=>{shareText();setShareMenu(false);}}><FileText size={16}/>Metni panoya kopyala</button><button disabled={!!busy} onClick={()=>{void exportNote();setShareMenu(false);}}><WinIcon name="ac_ic_save_as" size={17}/>Dosya olarak kaydet</button></div>}</div><button className="command-button" aria-label="Dışa aktar" title={detail.editable||richAvailable||!pagePreview.pages.some(p=>p.available)?'Metin dosyası olarak kaydet':'PDF olarak kaydet'} onClick={()=>void exportNote()} disabled={!!busy}><WinIcon name="ac_ic_save_as" size={17}/><span>Dosya olarak kaydet</span></button><button className="command-button optional-command" aria-label="Yazdır" onClick={()=>window.print()} title="Yazdır"><Printer size={16}/><span>Yazdır</span></button><button className="command-button optional-command" onClick={()=>setDocumentSearch(!documentSearch)}><Search size={16}/><span>Ara</span></button><button className="save-button" aria-busy={saving===selected||ink.saving} disabled={(ink.moveDirty&&!inkDirty&&!ink.eraseDirty?(ink.saving||ink.capturing||detail.syncState!=='synced'):ink.eraseDirty?(!ink.canSaveErase||ink.saving||ink.capturing):inkDirty?(!showingPage||!(ink.projection?.capabilities.appendInk||ink.projection?.capabilities.firstStroke||ink.projection?.capabilities.highlighter)||ink.saving||ink.capturing||ink.draft?.revision!==detail.revision):((!canEdit&&!canEditTitle&&!canRichEdit)||!draft?.dirty))||saving===selected||!!busy||readingMode} onClick={()=>void save()} title={`Kaydet (${shortcuts.label('saveNote')})`}>{saving===selected?<LoaderCircle className="spin" size={15}/>:<Save size={15}/>}<span>Kaydet</span></button><div className="dropdown-anchor"><button className="icon-button" aria-label="Not seçenekleri" onClick={()=>setNoteMenu(!noteMenu)} aria-expanded={noteMenu}><MoreVertical size={19}/></button>{noteMenu&&<div className="dropdown note-menu"><button disabled={!!busy||!!organizeReason(detail,{favorite:!detail.favorite})} title={organizeReason(detail,{favorite:!detail.favorite})||'Samsung Cloud favorilerini güncelle'} onClick={()=>{void organize({favorite:!detail.favorite});setNoteMenu(false);}}><WinIcon name="ic_drawer_favorite" size={17}/>{detail.favorite?'Favorilerden çıkar':'Favorilere ekle'}</button><button disabled={!!busy||!!organizeReason(detail,{trashed:!detail.trashed})} title={organizeReason(detail,{trashed:!detail.trashed})||'Samsung Cloud çöp kutusunu güncelle'} onClick={()=>{void organize({trashed:!detail.trashed});setNoteMenu(false);}}><WinIcon name="ic_drawer_delete" size={17}/>{detail.trashed?'Geri yükle':'Çöp kutusuna taşı'}</button><button disabled={!!busy||!!organizeReason(detail,{folderId:null})} title={organizeReason(detail,{folderId:null})||'Samsung Cloud klasörüne taşı'} onClick={()=>openMove([detail.id])}><Folder size={16}/>Klasöre taşı</button><button disabled={!!busy||detail.syncState!=='synced'} onClick={()=>openSorter()}><PanelTop size={16}/>Sayfaları düzenle</button>{detail.editable||richAvailable?<button disabled={!!busy} onClick={()=>{void duplicateNote();setNoteMenu(false);}}><FileText size={16}/>Kopyasını oluştur</button>:<button disabled={!!busy||!(draft?.body??detail.body??'').trim()} title={(draft?.body??detail.body??'').trim()?undefined:'Bu notta kopyalanacak metin yok.'} onClick={()=>{void copyNote();setNoteMenu(false);}}><FileText size={16}/>Metni yeni nota kopyala</button>}</div>}</div></div>}</header>
      {documentSearch&&<div className="document-search"><Search size={15}/><input aria-label="Bu notta ara" placeholder="Bu notta ara" onChange={e=>findInDocument(e.target.value)} onKeyDown={e=>{if(e.key==='Enter'){if(richAvailable)richEditor?.commands.focus();else bodyRef.current?.focus();}}}/><button className="icon-button" aria-label="Not aramasını kapat" onClick={()=>setDocumentSearch(false)}><X size={15}/></button></div>}
      {(ink.error||shapes.error||(ink.draft&&ink.projection&&ink.draft.revision!==ink.projection.revision))&&focusMode&&<div className="alert error" role="alert"><ShieldAlert size={18}/><span>{ink.error||shapes.error||'Notun sürümü değişti. Taslak çizgileriniz korunuyor; bu sürümün üzerine kaydedilmiyor.'}</span></div>}
      {error&&focusMode&&<div className="alert error" role="alert"><ShieldAlert size={18}/><span>{error}</span><button className="icon-button" aria-label="Uyarıyı kapat" onClick={()=>setError('')}><X size={15}/></button></div>}
      {pollError&&focusMode&&<div className="alert warning" role="status"><CloudOff size={17}/><span>{pollError}</span></div>}
      
      {status?.sync.error&&<div className="alert warning"><CloudOff size={17}/><span>{status.sync.error} Yerel kayıtlarınız korunuyor.</span></div>}
      {detailLoading?<div className="editor-empty"><LoaderCircle className="spin" size={23}/></div>:!detail?<div className="editor-empty"><p>Not seçin.</p></div>:<>
      {detail.editable&&editorInfo?.id===detail.id&&editorInfo.projection.reason&&!richAvailable&&!showingPage&&<p className="editor-format-reason">{editorInfo.projection.reason}</p>}
      {detail.error&&<div className="alert warning"><CloudOff size={17}/><span>{detail.error}</span></div>}
      {draft?.saveError&&draft.dirty&&<div className="alert warning"><span>Yazdıklarınız bu pencerede korunuyor.</span><button className="text-action" disabled={!!busy} onClick={()=>void copyNote()}>Yeni nota kopyala</button></div>}
      
      {detail.metadataSync?.error&&<div className="alert warning" role="alert"><CloudOff size={17}/><span>{detail.metadataSync.error}</span></div>}
      {trashReadOnly&&!detail.trashed&&<div className="alert warning"><Trash2 size={17}/><span>Bu not çöp kutusundaki bir klasörde. Klasör geri yüklenene kadar salt okunur gösterilir.</span></div>}
      {detail.trashed&&<div className="alert warning"><Trash2 size={17}/><span>Bu not çöp kutusunda.</span><button className="text-action" disabled={!!busy||!!organizeReason(detail,{trashed:false})} title={organizeReason(detail,{trashed:false})||'Notu Samsung Cloud üzerinde geri yükle'} onClick={()=>void organize({trashed:false})}>Geri yükle</button></div>}
      {!detail.editable&&!richAvailable&&!showingPage&&pagePreview.phase!=='loading'&&<p className="editor-format-reason" id="preview-limit">{(draft?.body??detail.body??'').trim()?'Bu notun sayfaları burada gösterilemiyor; yalnızca metni görünüyor.':'Bu notun sayfaları burada gösterilemiyor.'}</p>}
      {metadataNeedsResolution&&<div className="conflict-panel"><strong>Buluttaki düzenleme durumu doğrulanmalı.</strong><p>Buluttaki favori, klasör ve çöp kutusu durumu yeniden alınır. Bekleyen düzenleme isteği bırakılır; not metniniz korunur.</p><button className="small-button" disabled={!!busy||!!draft?.dirty} onClick={()=>void resolve('use_remote')}>Buluttaki durumu kullan</button>{draft?.dirty&&<p>Önce yazdıklarınızı kaydedin veya ayrı nota kopyalayın.</p>}</div>}
      {detail.syncState==='conflict'&&metadataState==='synced'&&<div className="conflict-panel"><strong>Bu not iki yerde değişmiş.</strong><p>Yerel sürümü ayrı bir not olarak saklayabilir veya buluttaki sürümü kullanabilirsiniz.</p>{detail.remoteBody!==undefined&&<details><summary>Buluttaki metni karşılaştır</summary><pre>{detail.remoteTitle}{'\n\n'}{detail.remoteBody}</pre></details>}<div className="conflict-actions">{draft?.dirty&&<button className="small-button" disabled={!!busy} onClick={()=>void copyNote()}>Yazdıklarımı yeni nota kopyala</button>}<button className="small-button" disabled={!!busy||!!draft?.dirty} onClick={()=>void resolve('keep_local_copy')}>Yerel sürümü kopya olarak sakla</button><button className="text-action" disabled={!!busy||!!draft?.dirty} onClick={()=>void resolve('use_remote')}>Buluttaki sürümü kullan</button></div></div>}
      
      {richAvailable&&pagesReady&&<div className="view-switch" role="group" aria-label="Görünüm"><button className={showingPage?'active':''} aria-pressed={showingPage} onClick={()=>setTextView(null)}>Sayfalar</button><button className={showingPage?'':'active'} aria-pressed={!showingPage} disabled={ink.hasDraft(detail.id)||ink.anySaving} title={ink.hasDraft(detail.id)||ink.anySaving?'Önce çizimleri kaydedin.':'Notun metnini biçimleriyle düzenle'} onClick={()=>{ink.setDrawing(false);shapes.setActive(false);setTextView(detail.id);}}>Metni düzenle</button>{!showingPage&&<span>El yazısı ve çizimler korunur; bu görünümde yalnız metin gösterilir.</span>}</div>}
      {!detail.editable&&showingPage&&<div className="page-navigation"><div>Sayfa <Select ref={pageSelectorRef} label="Gösterilen sayfa" value={String(pagePreview.pages.some(p=>p.index===pagePreview.pageIndex)?pagePreview.pageIndex:pagePreview.pages[0]?.index??'')} onChange={value=>{const index=Number(value);pagePreview.setPageIndex(index);setPageJump(previous=>({index,seq:(previous?.seq??0)+1}));}} options={pagePreview.pages.map(p=>{const shown=p.available||!!(p.width&&p.height);return {value:String(p.index),label:`${p.index+1} / ${pagePreview.pages.length}${shown?'':' — önizleme yok'}`,disabled:!shown};})}/></div></div>}
      
      <div className={`composer ${toolsOpen&&!trashReadOnly?'tools-open':''}`}><div className="canvas-scroll"><div className={`document ${showingPage?'page-document':''}`} style={{width:`${718*zoom/100}px`,...(showingPage?{backgroundColor:pagePreview.pages.find(p=>p.index===pagePreview.pageIndex)?.darkMode===true?'#000':'#fff'}:{})}}>{richAvailable&&draft?.editor&&!showingPage?<RichEditor key={detail.id} zoom={zoom} model={draft.editor} editable={canRichEdit&&!readingMode} onReady={setRichEditor} onError={setError} onChange={model=>setDrafts(previous=>({...previous,[detail.id]:{...previous[detail.id],editor:model,body:model.text,dirty:true,saveError:undefined}}))}/>:showingPage?<PageStack zoom={zoom} noteId={detail.id} revision={detail.revision} pages={pagePreview.pages} current={pagePreview.pageIndex} onCurrent={pagePreview.setPageIndex} jump={pageJump} pageSize={index=>{const known=ink.projection?.pages.find(item=>item.index===index)??shapes.page(index);return known?.width&&known.height?{width:known.width,height:known.height}:undefined;}} render={(page,url,isCurrent)=><><InkEditor controller={ink} readingMode={readingMode} maskColor={page.darkMode===true&&!preferences.darkPages?'#000':'#fff'} pageIndex={page.index} current={isCurrent} baselineComplete={page.complete!==false} blocked={inkDrawBlocked} imageUrl={url} onImageError={()=>{}}/><ShapeLayer controller={shapes} pageIndex={page.index} current={isCurrent} hasImage={!!url}/></>}/>:!detail.editable&&!richAvailable&&pagePreview.phase==='loading'?<div className="document-loading" aria-hidden="true"/>:<textarea ref={bodyRef} className="document-body" aria-label="Not içeriği" aria-describedby={!detail.editable?'preview-limit':undefined} value={draft?.body??detail.body??''} onChange={e=>updateDraft({body:e.target.value})} placeholder={!detail.editable?'Bu notta metin yok.':''} readOnly={!canEdit||readingMode} spellCheck={false} style={{fontSize:`${32*zoom/100}px`}}/>}</div><div className="zoom-controls"><button aria-label="Yakınlaştır" disabled={zoom>=150} onClick={()=>setZoom(Math.min(150,zoom+10))}><Plus size={13}/></button><span>{zoom}%</span><button aria-label="Uzaklaştır" disabled={zoom<=50} onClick={()=>setZoom(Math.max(50,zoom-10))}><Minus size={13}/></button></div></div>{!readingMode&&!trashReadOnly&&<TextTools ink={{highlighter:{available:showingPage&&inkHighlighterCanDraw,active:ink.drawing&&ink.tool==='highlighter',onToggle:()=>{if(ink.drawing&&ink.tool==='highlighter')ink.setDrawing(false);else{ink.setTool('highlighter');ink.setDrawing(true);}},reason:inkToolReason(ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked,'highlighter',showingPage),colorARGB:ink.highlighterStyle.colorARGB,size:ink.highlighterStyle.size,setStyle:ink.setHighlighterStyle},available:inkCanDraw||inTextOfPenNote,active:ink.drawing&&ink.tool==='pen'&&showingPage,onToggle:()=>{if(inTextOfPenNote){setTextView(null);ink.setTool('pen');ink.setDrawing(true);return;}if(ink.tool!=='pen'){ink.setTool('pen');ink.setDrawing(true);}else ink.setDrawing(value=>!value);},eraser:{available:showingPage&&canEraseInk(ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked),active:ink.drawing&&ink.tool==='eraser',onToggle:()=>{if(ink.drawing&&ink.tool==='eraser')ink.setDrawing(false);else{ink.setTool('eraser');ink.setDrawing(true);}},reason:inkToolReason(ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked,'eraser',showingPage)},select:{available:showingPage&&canSelectInk(ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked),active:ink.drawing&&ink.tool==='select',onToggle:()=>{if(ink.drawing&&ink.tool==='select')ink.setDrawing(false);else{ink.setTool('select');ink.setDrawing(true);}},reason:inkToolReason(ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked,'select',showingPage)},shapes:{available:showingPage&&shapes.available,active:shapes.active,onToggle:()=>{if(shapes.active)shapes.setActive(false);else{ink.setDrawing(false);shapes.setActive(true);}},reason:showingPage?shapes.reasonFor(pagePreview.pageIndex):'Şekil yalnız sayfa görünümünde eklenebilir.',panel:<ShapePanel controller={shapes}/>},penReason:inkToolReason(ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked,'pen',showingPage),onText:()=>{ink.setDrawing(false);shapes.setActive(false);if(richAvailable&&pagesReady&&detail&&!(ink.hasDraft(detail.id)||ink.anySaving))setTextView(detail.id);},canUndo:ink.canUndo,undo:ink.undo,canRedo:ink.canRedo,redo:ink.redo,undoReason:ink.undoReason,redoReason:ink.redoReason,pageMode:showingPage,styles:inkStyles(inkPage),firstStroke:pageDrawMode(ink.projection,inkPage)==='first',sourceId:ink.sourceId,chooseSource:ink.chooseSource,penStyle:ink.penStyle,setPenStyle:ink.setPenStyle,locked:!inkCanDraw||ink.capturing,reason:inkCanDraw||!inkPage?'':inkToolReason(ink,pagePreview.pageIndex,inkBaselineComplete,inkDrawBlocked,'pen',showingPage)}} editor={canRichEdit&&!readingMode?richEditor:null} open={toolsOpen} onToggle={()=>setToolsOpen(!toolsOpen)}/>}</div>
      <footer className="editor-footer" data-dirty={String(!!draft?.dirty)} data-ink-draft-count={ink.draft?.strokes.length??0} data-ink-pending={String(inkPending)} data-ink-move-count={ink.moveMark?.ids.length??0} data-ink-erase-count={ink.erasure?.ids.length??0}><span className={`cloud-state ${detail.syncState}`}><Cloud size={12}/>{syncLabels[detail.syncState]}</span><span>{(draft?.body??detail.body??'').trim().split(/\s+/).filter(Boolean).length} kelime</span></footer>
      </>}
    </main>
    {folderDialog&&<div className="modal-backdrop" onClick={()=>{if(!busy)setFolderDialog(null);}}><section ref={modalRef} className="settings-modal folder-dialog" role="dialog" aria-modal="true" aria-labelledby="folder-dialog-title" onClick={e=>e.stopPropagation()}><header><h2 id="folder-dialog-title">{folderDialogTitle}</h2><button className="icon-button" aria-label="Klasör penceresini kapat" disabled={!!busy} onClick={()=>setFolderDialog(null)}><X size={18}/></button></header><form onSubmit={e=>{e.preventDefault();void submitFolder();}}>{['create','rename'].includes(folderDialog.kind)&&<label className="folder-select-label">Klasör adı<input aria-label="Klasör adı" required maxLength={255} value={folderName} onChange={e=>setFolderName(e.target.value)}/></label>}{['create','move'].includes(folderDialog.kind)&&<div className="folder-select-label"><span aria-hidden="true">Konum</span><FolderPicker label="Üst klasör" options={parentOptions(folderDialog)} value={folderParent} onChange={setFolderParent} onChoose={setFolderParent} autoFocus={folderDialog.kind==='move'}/></div>}{['trash','restore'].includes(folderDialog.kind)&&<p className="settings-copy">“{folderDialog.folder?.name}” {folderDialog.kind==='trash'?'bulut çöp kutusuna taşınacak.':'bulut çöp kutusundan geri yüklenecek.'}</p>}{error&&<p className="folder-dialog-error" role="alert">{error}</p>}<div className="settings-actions"><button className="small-button" type="submit" disabled={!!busy||(['create','rename'].includes(folderDialog.kind)&&!folderName.trim())}>{folderDialog.kind==='create'?'Oluştur':folderDialog.kind==='trash'?'Çöp kutusuna taşı':folderDialog.kind==='restore'?'Geri yükle':'Kaydet'}</button><button className="text-action" type="button" disabled={!!busy} onClick={()=>setFolderDialog(null)}>Vazgeç</button></div></form></section></div>}
    {!!moveTargets.length&&<div className="modal-backdrop" onClick={()=>setMoveTargets([])}><section ref={modalRef} className="settings-modal move-modal" role="dialog" aria-modal="true" aria-labelledby="move-title" onClick={e=>e.stopPropagation()}><header><h2 id="move-title">Klasöre taşı</h2><button className="icon-button" aria-label="Klasör seçimini kapat" onClick={()=>setMoveTargets([])}><X size={19}/></button></header><p className="settings-copy">{moveTargets.length} notun klasörü değiştirilecek.</p><span className="folder-picker-label" aria-hidden="true">Klasör</span><FolderPicker label="Hedef klasör" options={moveOptions(moveTargets)} value={moveFolder} onChange={setMoveFolder} onChoose={id=>void moveNotes(id)} autoFocus/>{moveFolder===null&&<p className="move-reason">Bir klasör seçin.</p>}<div className="settings-actions"><button className="small-button" disabled={!!busy||moveFolder===null||!!moveTargetReason(moveTargets,moveFolder||null)} onClick={()=>void moveNotes()}>Taşı</button><button className="text-action" onClick={()=>setMoveTargets([])}>Vazgeç</button></div></section></div>}
    {contextMenu&&contextActions.length>0&&<ContextMenu key={`${contextMenu.kind}:${contextMenu.id}:${contextMenu.x}:${contextMenu.y}`} target={contextMenu} actions={contextActions} onClose={()=>setContextMenu(null)}/>}
    {sorter&&detail&&<PageSorter note={detail} pages={sorter.pages} busy={!!busy}
        blockedReason={!sorter.editable?sorter.reason:sorter.pending?'Önceki sayfa değişikliği eşitleniyor.':detail.syncState!=='synced'?'Önce bekleyen eşitleme tamamlanmalı.':null}
        canAdd={sorter.canAdd!==false}
        onReorder={ids=>pageOp({op:'reorder',pageIds:ids})}
        onDelete={ids=>pageOp({op:'delete',pageIds:ids})}
        onAdd={after=>pageOp({op:'add',afterIndex:after})}
        close={()=>setSorter(null)}/>}
    {settings&&<SettingsPanel status={status} foldersInitialized={foldersInitialized} offlineView={offlineView} busy={!!busy} error={syncError} preferenceError={preferenceError} onClose={()=>setSettings(false)} onSync={sync} sectionRef={modalRef} preferences={preferences} onPreferencesChange={changePreferences} shortcutSlot={<KeyboardShortcuts controller={shortcuts}/>} accountSlot={<AccountPanel status={status} busy={!!busy} error={accountError} onLogin={login} onDisconnect={disconnect}/>}/>}
    {noteDrag.ghost}
  </div>;
}
