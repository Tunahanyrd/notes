import {useContext,useEffect,useLayoutEffect,useRef,useState,type PointerEvent as ReactPointerEvent} from 'react';
import {api,ApiError,notePath,request} from './api';
import {useInkLive,type InkPenTemplate,type LivePenSpec,type LiveRaster} from './inkLive';
import {InkPageContext,inkTileBucket,snapInkOffset,visibleInkTiles} from './PageStack';
import type {Note} from './types';
import './ink-editor.css';

export interface InkPoint {x:number;y:number;pressure:number;timeMs:number}
// template:false marks a stroke the writer cannot copy (other pen/effects).
// layerId/erasable/bounds come from projection v4; bounds are page units.
export interface InkSource {id:string;kind:number|string;colorARGB:number;size:number;points?:InkPoint[];template?:boolean;layerId?:string;erasable?:boolean;bounds?:[number,number,number,number]}
// Per-page writer facts (projection): which edits this page allows and why not.
export interface InkPageCapabilities {appendInk?:boolean;eraseInk?:boolean;firstStroke?:boolean;highlighter?:boolean;reason?:string|null}
export interface InkPage {id:string;index:number;width:number;height:number;strokes:InkSource[];ink?:InkPageCapabilities;background?:'pdf'|null}
export interface InkStroke {id:string;pageId:string;sourceStrokeId:string;kind:number|string;colorARGB:number;size:number;points:InkPoint[];pageSize?:[number,number]}
// A queued (saved, not yet Cloud-verified) lasso move of saved strokes.
export interface InkMoveGroup {pageId:string;strokeIds:string[];dx:number;dy:number}
export interface InkProjection {schemaVersion:1;revision:number;pages:InkPage[];capabilities:{appendInk:boolean;eraseInk?:boolean;firstStroke?:boolean;highlighter?:boolean};reason:string|null;rendering:{nativeEquivalent:boolean;mode:string};pendingStrokes:InkStroke[];pendingErase?:{pageId:string;strokeIds:string[]}[];pendingMove?:InkMoveGroup[];optimistic?:boolean}
export interface InkDraft {revision:number;strokes:InkStroke[]}
export interface PenStyle {colorARGB?:number;size?:number}
// Simplified saved-stroke paths of one page (GET …/ink-geometry?page=…).
export interface InkGeometryStroke {id:string;layerId:string;size:number;points:[number,number][]}
export interface InkGeometry {schemaVersion:1;pageId:string;strokes:InkGeometryStroke[];reason:string|null}
// One Cloud delete per page/layer; the app owns the request (onSaveErase).
export interface InkEraseGroup {pageId:string;layerId:string;strokeIds:string[]}
export interface InkEraseRequest {noteId:string;expectedRevision:number;erase:InkEraseGroup[]}
export interface InkErasure {revision:number;ids:string[]}
// Saved strokes the lasso moved, not yet saved: one page, one id set, one offset.
export interface InkMoveMark {revision:number;pageId:string;ids:string[];dx:number;dy:number}
export interface InkMoveRequestGroup {pageId:string;layerId:string;strokeIds:string[];dx:number;dy:number}
export interface InkMoveRequest {noteId:string;expectedRevision:number;move:InkMoveRequestGroup[]}
export type InkTool='pen'|'eraser'|'select'|'highlighter';
// Session history. Draft actions are local; savedDraw is a stroke already
// handed to the Cloud queue (undo = erase it); savedErase cannot be undone
// (a deleted Cloud stroke cannot be restored); move is undone by moving back.
type InkAction={kind:'draw';stroke:InkStroke}|{kind:'erase';saved:string[];drafts:InkStroke[]}|{kind:'move';pageId:string;saved:string[];drafts:string[];dx:number;dy:number}|{kind:'savedDraw';ids:string[]}|{kind:'savedErase'};
export const MIN_PEN_SIZE=0.5,MAX_PEN_SIZE=64;
// Pen of a first stroke on a stroke-free page (recipe 4 writes a Samsung
// FountainPen); the panel's colour/size override these defaults.
export const FIRST_STROKE_COLOR=0xff252525,FIRST_STROKE_SIZE=2;
// Highlighter (recipe 5 writes a Samsung Marker4 top-layer stroke): alpha is
// fixed at 0x73 like Samsung's own UI; colour and page-unit size are free.
export const HIGHLIGHTER_ALPHA=0x73,HIGHLIGHTER_COLOR=0x73ffcb30,HIGHLIGHTER_SIZE=24,MIN_HIGHLIGHTER_SIZE=4,MAX_HIGHLIGHTER_SIZE=64;
export const isHighlighter=(stroke:{kind?:number|string})=>stroke.kind==='highlighter';
// A draft stroke without a template: the first stroke of an empty page.
export const isFirstStroke=(stroke:{sourceStrokeId:string;kind?:number|string})=>stroke.sourceStrokeId===''&&!isHighlighter(stroke);
const MAX_STROKES=64,MAX_POINTS=20_000,MAX_ERASE=64;
const pointCount=(strokes:InkStroke[])=>strokes.reduce((count,stroke)=>count+stroke.points.length,0);
const sameSet=(a:string[],b:string[])=>a.length===b.length&&a.every(id=>b.includes(id));
function checkedProjection(value:InkProjection,revision:number):InkProjection {
  if(value.schemaVersion!==1||value.revision!==revision||!Array.isArray(value.pages)||value.pages.length>2000||!Array.isArray(value.pendingStrokes))throw Error('invalid_ink_projection');
  const pageIds=new Set<string>();
  for(const page of value.pages){if(typeof page.id!=='string'||pageIds.has(page.id)||!Number.isInteger(page.index)||page.index<0||!Number.isFinite(page.width)||!Number.isFinite(page.height)||page.width<=0||page.height<=0||!Array.isArray(page.strokes))throw Error('invalid_ink_page');pageIds.add(page.id);for(const source of page.strokes)if(typeof source.id!=='string'||(source.template!==false&&(!Number.isFinite(source.size)||source.size<=0||!Number.isInteger(source.colorARGB))))throw Error('invalid_ink_source');}
  if(value.pendingMove!==undefined&&(!Array.isArray(value.pendingMove)||value.pendingMove.some(group=>typeof group.pageId!=='string'||!Array.isArray(group.strokeIds)||!Number.isFinite(group.dx)||!Number.isFinite(group.dy))))throw Error('invalid_ink_projection');
  return value;
}
function checkedGeometry(value:InkGeometry,pageId:string):InkGeometry {
  if(value.schemaVersion!==1||value.pageId!==pageId||!Array.isArray(value.strokes)||value.strokes.length>20000)throw Error('invalid_ink_geometry');
  for(const stroke of value.strokes)if(typeof stroke.id!=='string'||typeof stroke.layerId!=='string'||!Number.isFinite(stroke.size)||!Array.isArray(stroke.points)||stroke.points.some(point=>!Array.isArray(point)||point.length!==2||!point.every(Number.isFinite)))throw Error('invalid_ink_geometry');
  return value;
}
// Squared distance between segments ab and cd (stroke eraser hit test).
function segmentDistance2(ax:number,ay:number,bx:number,by:number,cx:number,cy:number,dx:number,dy:number){
  const point=(px:number,py:number,sx:number,sy:number,ex:number,ey:number)=>{const vx=ex-sx,vy=ey-sy,len=vx*vx+vy*vy;const t=len?Math.max(0,Math.min(1,((px-sx)*vx+(py-sy)*vy)/len)):0;const qx=sx+t*vx-px,qy=sy+t*vy-py;return qx*qx+qy*qy;};
  const d1=(bx-ax)*(cy-ay)-(by-ay)*(cx-ax),d2=(bx-ax)*(dy-ay)-(by-ay)*(dx-ax),d3=(dx-cx)*(ay-cy)-(dy-cy)*(ax-cx),d4=(dx-cx)*(by-cy)-(dy-cy)*(bx-cx);
  if(((d1>0&&d2<0)||(d1<0&&d2>0))&&((d3>0&&d4<0)||(d3<0&&d4>0)))return 0;
  return Math.min(point(ax,ay,cx,cy,dx,dy),point(bx,by,cx,cy,dx,dy),point(cx,cy,ax,ay,bx,by),point(dx,dy,ax,ay,bx,by));
}
// True when the eraser path (page units) comes within radius+size/2 of the
// stroke path. Tapping is a zero-length segment.
export function strokeHit(path:[number,number][],points:[number,number][],size:number,radius:number){
  if(!path.length||!points.length)return false;
  const limit=(radius+Math.max(0,size)/2)**2;
  const eraser=path.length===1?[path[0],path[0]]:path,line=points.length===1?[points[0],points[0]]:points;
  for(let i=1;i<eraser.length;i++)for(let j=1;j<line.length;j++)if(segmentDistance2(eraser[i-1][0],eraser[i-1][1],eraser[i][0],eraser[i][1],line[j-1][0],line[j-1][1],line[j][0],line[j][1])<=limit)return true;
  return false;
}
function insidePolygon(x:number,y:number,polygon:[number,number][]){let inside=false;for(let i=0,j=polygon.length-1;i<polygon.length;j=i++){const [xi,yi]=polygon[i],[xj,yj]=polygon[j];if((yi>y)!==(yj>y)&&x<(xj-xi)*(y-yi)/(yj-yi)+xi)inside=!inside;}return inside;}
// Lasso: a stroke is selected when at least half of its points lie inside.
export function lassoHit(polygon:[number,number][],points:[number,number][]){if(polygon.length<3||!points.length)return false;const step=Math.max(1,Math.floor(points.length/200));let inside=0,total=0;for(let i=0;i<points.length;i+=step){total++;if(insidePolygon(points[i][0],points[i][1],polygon))inside++;}return inside*2>=total;}
const shifted=(strokes:InkStroke[],ids:Set<string>,dx:number,dy:number)=>strokes.map(stroke=>ids.has(stroke.id)?{...stroke,points:stroke.points.map(point=>({...point,x:point.x+dx,y:point.y+dy}))}:stroke);
// Offset of saved strokes already queued for a Cloud move (pendingMove).
export function queuedOffsets(projection:InkProjection|null){const offsets=new Map<string,[number,number]>();for(const group of projection?.pendingMove??[])for(const id of group.strokeIds)offsets.set(id,[group.dx,group.dy]);return offsets;}
export function useInkDrafts(note:Note|null,enabled:boolean,onSaved:(note:Note)=>void,options:{onSaveErase?:(request:InkEraseRequest)=>Promise<Note>;onSaveMove?:(request:InkMoveRequest)=>Promise<Note>}={}) {
  const [drafts,setDrafts]=useState<Record<string,InkDraft>>({});
  const [drawing,setDrawing]=useState(false);
  const [tool,setToolState]=useState<InkTool>('pen');
  const [capturing,updateCapturing]=useState(false);const captureLock=useRef(false);const pendingAccept=useRef<(()=>void)|null>(null);
  const setCapturing=(value:boolean)=>{captureLock.current=value;updateCapturing(value);if(!value&&pendingAccept.current){const accept=pendingAccept.current;pendingAccept.current=null;accept();}};
  const [loaded,setLoaded]=useState<{noteId:string;projection:InkProjection}|null>(null);
  const [phase,setPhase]=useState<'idle'|'loading'|'ready'|'unavailable'>('idle');
  const [error,setError]=useState('');const [saving,setSaving]=useState<string|null>(null);
  // Our previous batch still syncing (409 sync_pending): nothing was accepted,
  // so the draft is saved again once the note is synced. `waitTick` re-arms
  // the autosave every 2 s; after 10 minutes it gives up with an error.
  const [waitTick,setWaitTick]=useState(0);const waitingSince=useRef<number|null>(null);
  // Saved strokes the eraser marked, per note, bound to the projection revision.
  const [erasures,setErasures]=useState<Record<string,InkErasure>>({});
  const [moves,setMoves]=useState<Record<string,InkMoveMark>>({});
  const [geometry,setGeometry]=useState<Record<string,{status:'loading'|'ready'|'failed';strokes:InkGeometryStroke[]}>>({});
  // Saved pens of template strokes (live preview); kept across revisions of
  // the same note so a just-saved line never falls back while they reload.
  const [pens,setPens]=useState<{noteId:string;pens:Record<string,InkPenTemplate>}|null>(null);
  const active=useRef({note,drafts,loaded,onSaved,erasures,moves,options});active.current={note,drafts,loaded,onSaved,erasures,moves,options};
  const saveLock=useRef(false);
  const uncertainTransfer=useRef(new Set<string>());const [,setUncertainVersion]=useState(0);
  const batchReady=useRef<{id:string;ready:Promise<void>}|null>(null);
  const pendingAppends=useRef(new Map<string,Set<Promise<void>>>());
  // Per-note pen source and undo/redo history. History lives for the app
  // session; every entry is re-checked against the current note before use.
  const [sources,setSources]=useState<Record<string,string>>({});
  const [histories,setHistories]=useState<Record<string,InkAction[]>>({});
  const [futures,setFutures]=useState<Record<string,InkAction[]>>({});
  // Client draft ids of the last saved batch, matched to the queued Cloud ids
  // (same order) once the next projection arrives.
  const [savedBatches,setSavedBatches]=useState<Record<string,{revision:number;clientIds:string[];strokes:InkStroke[];firstPageId?:string}>>({});
  // Optional colour/size override of the chosen template (recipe 3 on save).
  const [penStyles,setPenStyles]=useState<Record<string,PenStyle>>({});
  const [highlighterStyle,setHighlighterStyleState]=useState<{colorARGB:number;size:number}>({colorARGB:HIGHLIGHTER_COLOR,size:HIGHLIGHTER_SIZE});
  useEffect(()=>{setDrawing(false);},[note?.id]);
  useEffect(()=>{setError(note&&uncertainTransfer.current.has(note.id)?'Çizimin durumunu doğrulamak için uygulamayı yeniden açın.':'');},[enabled,note?.id,note?.revision]);
  useEffect(()=>{
    if(!enabled||!note){setDrawing(false);setPhase('idle');setLoaded(null);return;}
    const controller=new AbortController();const id=note.id,revision=note.revision;
    const previous=active.current.loaded,batch=savedBatches[id];
    const ours=previous?.noteId===id&&(previous.projection.revision===revision||(batch?.revision===revision&&previous.projection.revision===revision-1));
    if(!ours){setPhase('loading');setLoaded(null);}
    void api<InkProjection>(notePath(id)+'/ink','GET',undefined,controller.signal).then(value=>{const projection=checkedProjection(value,revision);if(!controller.signal.aborted){setLoaded({noteId:id,projection});setPhase('ready');}}).catch(()=>{if(!controller.signal.aborted)setPhase('unavailable');});
    return()=>controller.abort();
  },[enabled,note?.id,note?.revision,note?.syncState]);
  const localBatch=note?savedBatches[note.id]:undefined;
  const projection:InkProjection|null=loaded&&loaded.noteId===note?.id?(loaded.projection.revision===note?.revision?loaded.projection:
    localBatch&&localBatch.revision===note?.revision&&loaded.projection.revision===localBatch.revision-1?{...loaded.projection,revision:localBatch.revision,pendingStrokes:localBatch.strokes,optimistic:true}:null):null;
  useEffect(()=>{
    if(!enabled||!note||!projection)return;
    const controller=new AbortController(),id=note.id,revision=projection.revision;
    void api<{schemaVersion:number;revision:number;pens:Record<string,InkPenTemplate>}>(`${notePath(id)}/ink-pens?revision=${revision}`,'GET',undefined,controller.signal).then(value=>{
      if(controller.signal.aborted||value?.schemaVersion!==1||value.revision!==revision||!value.pens||typeof value.pens!=='object'||Array.isArray(value.pens))return;
      setPens(previous=>({noteId:id,pens:{...(previous?.noteId===id?previous.pens:{}),...value.pens}}));
    }).catch(()=>{});
    return()=>controller.abort();
  },[enabled,note?.id,projection?.revision]);
  // Geometry belongs to one note revision; a new revision reloads on demand.
  useEffect(()=>{setGeometry({});},[note?.id,projection?.revision]);
  // Every draft change is written to this device at once (never to Cloud), so
  // leaving the note or closing the app cannot lose lines drawn before the
  // autosave. A note opened without an in-memory draft restores it from disk.
  const persisted=useRef(new Map<string,InkDraft|undefined>());
  // Serialize device draft writes and batch handoff per note.
  const draftQueue=useRef(new Map<string,Promise<void>>());
  const transferred=useRef(new Map<string,Set<string>>());
  const restoring=useRef(new Map<string,InkDraft>());const loadingDraft=useRef(new Set<string>());const [,setRestoreVersion]=useState(0);
  const serializeDraft=<T,>(id:string,work:()=>Promise<T>):Promise<T>=>{
    const previous=draftQueue.current.get(id)??Promise.resolve();
    const result=previous.catch(()=>{}).then(work);
    const tail=result.then(()=>{},()=>{});draftQueue.current.set(id,tail);
    void tail.then(()=>{if(draftQueue.current.get(id)===tail)draftQueue.current.delete(id);});
    return result;
  };
  const persistDraft=async(id:string,value:InkDraft|undefined)=>{
    const owned=transferred.current.get(id),current=value?.strokes??[],stored=restoring.current.get(id),restored=stored?.strokes??[];
    const ids=new Set(current.map(stroke=>stroke.id));
    const strokes=[...restored.filter(stroke=>!ids.has(stroke.id)),...current].filter(stroke=>!owned?.has(stroke.id));
    const revision=stored?Math.min(value?.revision??stored.revision,stored.revision):value?.revision??0;
    await api(notePath(id)+'/ink-draft','PUT',{revision,strokes});
    if(stored&&restored.every(stroke=>ids.has(stroke.id)||owned?.has(stroke.id))){restoring.current.delete(id);setRestoreVersion(v=>v+1);}
  };
  useEffect(()=>{
    for(const id of new Set([...Object.keys(drafts),...persisted.current.keys()])){
      const value=drafts[id];
      if(persisted.current.get(id)===value){
        const stored=restoring.current.get(id);
        if(stored&&value&&stored.strokes.every(stroke=>value.strokes.some(item=>item.id===stroke.id))){restoring.current.delete(id);setRestoreVersion(v=>v+1);}
        continue;
      }
      persisted.current.set(id,value);
      void serializeDraft(id,async()=>{await Promise.all([...(pendingAppends.current.get(id)??[])]);if(!uncertainTransfer.current.has(id))await persistDraft(id,value);}).catch(()=>{if(active.current.note?.id===id)setError('Çizim taslağı bu cihaza yazılamadı. Uygulamayı kapatmadan önce çizimleri kaydedin.');});
    }
  },[drafts]);
  useEffect(()=>{
    if(!note||!projection||drafts[note.id]||persisted.current.has(note.id))return;
    const id=note.id;persisted.current.set(id,undefined);loadingDraft.current.add(id);setRestoreVersion(v=>v+1);
    void serializeDraft(id,async()=>{
      try{
        const value=await api<InkDraft|{revision:null;strokes:InkStroke[]}>(notePath(id)+'/ink-draft','GET');
        if(value.revision!==null&&value.strokes.length)restoring.current.set(id,{revision:value.revision,strokes:value.strokes});
        return value;
      }catch(error){uncertainTransfer.current.add(id);setUncertainVersion(v=>v+1);if(active.current.note?.id===id)setError('Çizimin durumunu doğrulamak için uygulamayı yeniden açın.');throw error;}
    }).then(value=>{
      if(value.revision===null||!value.strokes.length)return;
      const restored={revision:value.revision,strokes:value.strokes} as InkDraft;
      persisted.current.set(id,restored);
      setDrafts(previous=>{const current=previous[id];if(!current)return {...previous,[id]:restored};const ids=new Set(current.strokes.map(stroke=>stroke.id));return {...previous,[id]:{...current,revision:Math.min(current.revision,restored.revision),strokes:[...restored.strokes.filter(stroke=>!ids.has(stroke.id)),...current.strokes]}};});
    }).catch(()=>{}).finally(()=>{loadingDraft.current.delete(id);setRestoreVersion(v=>v+1);});
  },[note?.id,projection?.revision]);
  // Our own pending batch finishing bumps the note revision. A draft drawn
  // meanwhile moves to the new revision when its pages and template strokes
  // still exist; Cloud compare still rejects a real concurrent change. Marked
  // erasures and moves follow the same way when their strokes still exist.
  useEffect(()=>{
    if(!note||!projection)return;
    const pages=new Map(projection.pages.map(page=>[page.id,page]));
    const current=drafts[note.id];
    if(!projection.optimistic&&current&&current.revision<=projection.revision){
      const batch=savedBatches[note.id];
      const rebased=current.strokes.map(stroke=>{
        if(!isFirstStroke(stroke))return stroke;
        const page=pages.get(stroke.pageId);
        const queued=projection.optimistic?undefined:projection.pendingStrokes.find(item=>item.pageId===stroke.pageId&&!item.sourceStrokeId&&!isHighlighter(item));
        // If sync already finished before the projection arrived, the only
        // new source on our own first-stroke page is its verified template.
        const verified=batch?.firstPageId===stroke.pageId&&batch.revision===projection.revision&&!projection.pendingStrokes.length&&page?.strokes.length===1?page.strokes[0]:undefined;
        const source=queued??verified;return source?{...stroke,sourceStrokeId:source.id}:stroke;
      });
      const intact=(stroke:InkStroke)=>{const page=pages.get(stroke.pageId);const queued=projection.pendingStrokes.some(item=>item.pageId===stroke.pageId&&item.id===stroke.sourceStrokeId&&!item.sourceStrokeId);return !!page&&(isHighlighter(stroke)||page.strokes.some(source=>source.id===stroke.sourceStrokeId)||queued||(isFirstStroke(stroke)&&!page.strokes.length&&page.ink?.firstStroke===true))&&(!stroke.pageSize||(stroke.pageSize[0]===page.width&&stroke.pageSize[1]===page.height));};
      if(rebased.every(intact)&&(current.revision<projection.revision||rebased.some((stroke,i)=>stroke!==current.strokes[i]))){setDrafts(previous=>previous[note.id]===current?{...previous,[note.id]:{...current,revision:projection.revision,strokes:rebased}}:previous);setError('');}
    }
    const erasable=new Set(projection.pages.flatMap(page=>page.strokes.filter(stroke=>stroke.erasable===true).map(stroke=>stroke.id)));
    const marked=erasures[note.id];
    if(marked&&marked.revision<projection.revision&&marked.ids.every(id=>erasable.has(id)))setErasures(previous=>previous[note.id]===marked?{...previous,[note.id]:{...marked,revision:projection.revision}}:previous);
    const moved=moves[note.id];
    if(moved&&moved.revision<projection.revision&&moved.ids.every(id=>erasable.has(id)))setMoves(previous=>previous[note.id]===moved?{...previous,[note.id]:{...moved,revision:projection.revision}}:previous);
    // Drafts just saved become queued Cloud strokes: undo then erases them.
    const batch=savedBatches[note.id];
    if(batch&&!projection.optimistic&&batch.revision<=projection.revision){
      const server=projection.pendingStrokes.map(stroke=>stroke.id);
      const map=batch.revision===projection.revision&&server.length===batch.clientIds.length?new Map(batch.clientIds.map((id,index)=>[id,server[index]])):null;
      const convert=(items:InkAction[]|undefined)=>(items??[]).flatMap((item):InkAction[]=>{
        if(item.kind==='draw'&&batch.clientIds.includes(item.stroke.id))return map?[{kind:'savedDraw',ids:[map.get(item.stroke.id)!]}]:[];
        if(item.kind==='move'&&item.drafts.some(id=>batch.clientIds.includes(id))){if(!map)return [];return [{...item,saved:[...item.saved,...item.drafts.map(id=>map.get(id)!)],drafts:[]}];}
        return [item];
      });
      setHistories(previous=>({...previous,[note.id]:convert(previous[note.id])}));
      setSavedBatches(previous=>{const next={...previous};delete next[note.id];return next;});
    }
  },[note?.id,projection?.revision,projection?.optimistic,note?drafts[note.id]:undefined]);
  const draft=note?drafts[note.id]:undefined;
  const erasure=note?erasures[note.id]:undefined;
  const moveMark=note?moves[note.id]:undefined;
  const write=(target:Note,strokes:InkStroke[])=>setDrafts(previous=>{const next={...previous};if(strokes.length)next[target.id]={revision:previous[target.id]?.revision??target.revision,strokes};else delete next[target.id];return next;});
  const writeErasure=(target:Note,ids:string[])=>setErasures(previous=>{const next={...previous};if(ids.length)next[target.id]={revision:previous[target.id]?.revision??projection?.revision??target.revision,ids};else delete next[target.id];return next;});
  const strokesNow=note?drafts[note.id]?.strokes??[]:[];
  const erasedNow=erasure?.ids??[];
  const historyNow=note?histories[note.id]??[]:[];
  const futureNow=note?futures[note.id]??[]:[];
  const record=(id:string,items:InkAction[],future:InkAction[])=>{setHistories(previous=>({...previous,[id]:items}));setFutures(previous=>({...previous,[id]:future}));};
  const pageOf=(id:string)=>projection?.pages.find(page=>page.strokes.some(stroke=>stroke.id===id));
  // Why saved strokes cannot be moved by (dx,dy) now; '' when they can.
  const moveProblem=(pageId:string,ids:string[],dx:number,dy:number)=>{
    if(!ids.length)return '';
    if(!projection)return 'Çizim verisi henüz hazır değil.';
    if(projection.capabilities.eraseInk!==true)return 'Bu notta kayıtlı çizgiler taşınamıyor.';
    const page=projection.pages.find(item=>item.id===pageId);if(!page)return 'Sayfa bu sürümde yok.';
    const queued=queuedOffsets(projection);
    if(moveMark&&(moveMark.pageId!==pageId||!sameSet(moveMark.ids,ids)))return 'Önceki taşıma kaydedilmeden başka çizgiler taşınamaz.';
    if(ids.length>MAX_ERASE)return 'Tek seferde en çok 64 çizgi taşınabilir.';
    for(const id of ids){
      const stroke=page.strokes.find(item=>item.id===id);
      if(!stroke){if(projection.pendingStrokes.some(item=>item.id===id))return 'Çizgi buluta eşitleniyor; eşitlenince taşınabilir.';return 'Çizgi bu sürümde bulunamadı.';}
      if(stroke.erasable!==true||!stroke.layerId)return 'Bu çizgi taşınamıyor.';
      if(erasedNow.includes(id))return 'Silinmek üzere işaretli çizgi taşınamaz.';
      const [qx,qy]=queued.get(id)??[0,0],mx=moveMark?.dx??0,my=moveMark?.dy??0;
      const box=stroke.bounds;if(box&&(box[0]+qx+mx+dx<0||box[2]+qx+mx+dx>page.width||box[1]+qy+my+dy<0||box[3]+qy+my+dy>page.height))return 'Taşıma sayfanın dışına çıkıyor.';
    }
    return '';
  };
  const markMove=(target:Note,pageId:string,ids:string[],dx:number,dy:number)=>setMoves(previous=>{
    const next={...previous},current=previous[target.id];
    if(current){const x=current.dx+dx,y=current.dy+dy;if(!x&&!y)delete next[target.id];else next[target.id]={...current,dx:x,dy:y};}
    else next[target.id]={revision:projection?.revision??target.revision,pageId,ids:[...ids],dx,dy};
    return next;
  });
  // Why the saved strokes cannot be marked for erase now; '' when they can.
  const eraseProblem=(ids:string[])=>{
    if(!projection)return 'Çizim verisi henüz hazır değil.';
    if(projection.capabilities.eraseInk!==true)return 'Bu notta kayıtlı çizgiler silinemiyor.';
    for(const id of ids){
      const page=pageOf(id);
      if(!page){if(projection.pendingStrokes.some(item=>item.id===id))return 'Çizgi buluta eşitleniyor; eşitlenince geri alınabilir.';return 'Çizgi bu sürümde bulunamadı.';}
      if(page.strokes.find(item=>item.id===id)?.erasable!==true)return 'Bu çizgi silinemiyor.';
      if(moveMark?.ids.includes(id))return 'Taşınmak üzere işaretli çizgi silinemez.';
      const remaining=page.strokes.filter(item=>!erasedNow.includes(item.id)&&!ids.includes(item.id)).length;
      if(!remaining)return 'Sayfadaki son kayıtlı çizgi silinemez.';
    }
    if(erasedNow.length+ids.filter(id=>!erasedNow.includes(id)).length>MAX_ERASE)return 'Önce silinen çizgileri kaydedin.';
    return '';
  };
  const actionProblem=(action:InkAction,direction:'undo'|'redo'):string=>{
    const sign=direction==='undo'?-1:1;
    switch(action.kind){
      case 'draw':if(direction==='undo')return strokesNow.some(stroke=>stroke.id===action.stroke.id)?'':'Bu çizgi artık taslakta değil.';
        return !projection||projection.pages.some(page=>page.id===action.stroke.pageId&&(isHighlighter(action.stroke)||page.strokes.some(source=>source.id===action.stroke.sourceStrokeId)))?'':'Bu çizgi artık geri getirilemiyor.';
      case 'erase':if(direction==='undo')return action.saved.every(id=>erasedNow.includes(id))?'':'Silinen çizgiler kaydedildi; geri getirilemez.';
        return action.saved.length?eraseProblem(action.saved.filter(id=>!erasedNow.includes(id))):'';
      case 'move':if(!action.drafts.every(id=>strokesNow.some(stroke=>stroke.id===id)))return 'Taşınan çizgi artık taslakta değil.';
        return moveProblem(action.pageId,action.saved,sign*action.dx,sign*action.dy);
      case 'savedDraw':if(direction==='undo')return eraseProblem(action.ids);
        return action.ids.every(id=>erasedNow.includes(id))?'':'Silme kaydedildi; yinelenemez.';
      case 'savedErase':return 'Kaydedilen silme geri alınamaz.';
    }
  };
  const applyAction=(target:Note,action:InkAction,direction:'undo'|'redo')=>{
    const sign=direction==='undo'?-1:1;
    if(action.kind==='draw'){if(direction==='undo')write(target,strokesNow.filter(stroke=>stroke.id!==action.stroke.id));else write(target,[...strokesNow,action.stroke]);}
    else if(action.kind==='erase'){
      if(direction==='undo'){if(action.drafts.length)write(target,[...strokesNow,...action.drafts]);if(action.saved.length)writeErasure(target,erasedNow.filter(id=>!action.saved.includes(id)));}
      else{const ids=new Set(action.drafts.map(stroke=>stroke.id));if(ids.size)write(target,strokesNow.filter(stroke=>!ids.has(stroke.id)));if(action.saved.length)writeErasure(target,[...erasedNow,...action.saved.filter(id=>!erasedNow.includes(id))]);}
    }
    else if(action.kind==='move'){if(action.drafts.length)write(target,shifted(strokesNow,new Set(action.drafts),sign*action.dx,sign*action.dy));if(action.saved.length)markMove(target,action.pageId,action.saved,sign*action.dx,sign*action.dy);}
    else if(action.kind==='savedDraw'){if(direction==='undo')writeErasure(target,[...erasedNow,...action.ids.filter(id=>!erasedNow.includes(id))]);else writeErasure(target,erasedNow.filter(id=>!action.ids.includes(id)));}
  };
  const busyReason=note&&uncertainTransfer.current.has(note.id)?'Notu yeniden açın.':saveLock.current||saving?'Çizimler kaydediliyor; biraz bekleyin.':captureLock.current||capturing?'Çizim sürüyor.':'';
  const lastAction=historyNow[historyNow.length-1],nextAction=futureNow[futureNow.length-1];
  const undoReason=!note?'Geri alınacak işlem yok.':busyReason||(lastAction?actionProblem(lastAction,'undo'):strokesNow.length?'':'Geri alınacak çizim işlemi yok.');
  const redoReason=!note?'Yinelenecek işlem yok.':busyReason||(nextAction?actionProblem(nextAction,'redo'):'Yinelenecek çizim işlemi yok.');
  // Kept for callers that replace the whole draft; history then restarts.
  const change=(strokes:InkStroke[])=>{if(!note||saveLock.current)return;write(note,strokes);record(note.id,[],[]);setError('');};
  const draw=(stroke:InkStroke)=>{if(!note)return;
    const barrier=batchReady.current;
    if((saveLock.current&&barrier?.id===note.id)||uncertainTransfer.current.has(note.id)||loadingDraft.current.has(note.id)){
      const id=note.id,revision=drafts[id]?.revision??(projection?.optimistic?projection.revision-1:note.revision);
      const pending=(barrier?.id===id?barrier.ready:Promise.resolve()).then(()=>api(notePath(id)+'/ink-draft','POST',{revision,strokes:[stroke]})).then(()=>{},()=>{if(active.current.note?.id===id)setError('Çizim taslağı bu cihaza yazılamadı.');});
      const set=pendingAppends.current.get(id)??new Set<Promise<void>>();set.add(pending);pendingAppends.current.set(id,set);
      void pending.then(()=>{set.delete(pending);if(!set.size)pendingAppends.current.delete(id);});
    }
    setDrafts(previous=>({...previous,[note.id]:{revision:previous[note.id]?.revision??(projection?.optimistic?projection.revision-1:note.revision),strokes:[...(previous[note.id]?.strokes??[]),stroke]}}));setHistories(previous=>({...previous,[note.id]:[...(previous[note.id]??[]),{kind:'draw',stroke}]}));setFutures(previous=>({...previous,[note.id]:[]}));if(!uncertainTransfer.current.has(note.id))setError('');};
  // Stroke eraser: saved strokes are only marked here (saved by saveErase);
  // unsaved draft strokes are removed from the draft at once.
  const erase=(savedIds:string[],draftIds:string[])=>{
    if(!note||saveLock.current||(!savedIds.length&&!draftIds.length))return;
    const removed=strokesNow.filter(stroke=>draftIds.includes(stroke.id));
    const saved=savedIds.filter(id=>!erasedNow.includes(id)&&!moveMark?.ids.includes(id));
    if(savedIds.some(id=>moveMark?.ids.includes(id)))setError('Taşınmak üzere işaretli çizgiler, taşıma kaydedilince silinebilir.');else setError('');
    if(erasedNow.length+saved.length>MAX_ERASE){setError('Önce silinen çizgileri kaydedin.');return;}
    if(!removed.length&&!saved.length)return;
    if(removed.length)write(note,strokesNow.filter(stroke=>!draftIds.includes(stroke.id)));
    if(saved.length)writeErasure(note,[...erasedNow,...saved]);
    record(note.id,[...historyNow,{kind:'erase',saved,drafts:removed}],[]);
  };
  // Lasso move: draft strokes shift at once; saved strokes get one move mark
  // (saved by saveMove). Offsets are whole page units. Returns a refusal.
  const move=(pageId:string,savedIds:string[],draftIds:string[],dx:number,dy:number):string=>{
    if(!note||saveLock.current)return 'Çizimler kaydediliyor; biraz bekleyin.';
    dx=Math.round(dx);dy=Math.round(dy);if(!dx&&!dy||(!savedIds.length&&!draftIds.length))return '';
    const drafts=draftIds.filter(id=>strokesNow.some(stroke=>stroke.id===id));
    const problem=moveProblem(pageId,savedIds,dx,dy);if(problem){setError(problem);return problem;}
    if(drafts.length)write(note,shifted(strokesNow,new Set(drafts),dx,dy));
    if(savedIds.length)markMove(note,pageId,savedIds,dx,dy);
    record(note.id,[...historyNow,{kind:'move',pageId,saved:[...savedIds],drafts,dx,dy}],[]);setError('');return '';
  };
  const undo=()=>{
    if(!note||undoReason)return;
    if(!lastAction){
      // Draft restored from disk has no history: undo its strokes in order.
      const stroke=strokesNow[strokesNow.length-1];write(note,strokesNow.slice(0,-1));record(note.id,[],[...futureNow,{kind:'draw',stroke}]);setError('');return;
    }
    applyAction(note,lastAction,'undo');record(note.id,historyNow.slice(0,-1),[...futureNow,lastAction]);setError('');
  };
  const redo=()=>{
    if(!note||redoReason||!nextAction)return;
    applyAction(note,nextAction,'redo');record(note.id,[...historyNow,nextAction],futureNow.slice(0,-1));setError('');
  };
  const chooseSource=(id:string)=>{if(note&&!captureLock.current){setSources(previous=>({...previous,[note.id]:id}));setPenStyles(previous=>{const next={...previous};delete next[note.id];return next;});}};
  const setPenStyle=(style:PenStyle)=>{if(note&&!captureLock.current)setPenStyles(previous=>({...previous,[note.id]:{...previous[note.id],...style}}));};
  const setTool=(value:InkTool)=>{if(!captureLock.current)setToolState(value);};
  const setHighlighterStyle=(style:{colorARGB?:number;size?:number})=>{if(!captureLock.current)setHighlighterStyleState(previous=>({...previous,...style}));};
  const loadGeometry=(pageId:string)=>{
    const current=active.current.note;if(!current||!projection||geometry[pageId])return;
    const revision=projection.revision,id=current.id;
    setGeometry(previous=>({...previous,[pageId]:{status:'loading',strokes:[]}}));
    void api<InkGeometry>(`${notePath(id)}/ink-geometry?page=${encodeURIComponent(pageId)}&revision=${revision}`).then(value=>{
      const checked=checkedGeometry(value,pageId);
      if(active.current.note?.id===id&&active.current.loaded?.projection.revision===revision)setGeometry(previous=>({...previous,[pageId]:{status:checked.reason?'failed':'ready',strokes:checked.strokes}}));
    }).catch(()=>{if(active.current.note?.id===id)setGeometry(previous=>({...previous,[pageId]:{status:'failed',strokes:[]}}));});
  };
  const save=async()=>{
    const {note:current,drafts:all,loaded:latest,onSaved:accept}=active.current;
    if(!current||saveLock.current||captureLock.current||uncertainTransfer.current.has(current.id)||loadingDraft.current.has(current.id)||restoring.current.has(current.id))return;
    const all_=all[current.id];
    // Pen strokes go first as one batch; every highlighter stroke is its own
    // queued write (recipe 5) once the note is synced again.
    const firstItem=all_?.strokes[0];
    const marker=firstItem&&isHighlighter(firstItem)?firstItem:undefined;
    const batch=firstItem&&(marker||isFirstStroke(firstItem))?[firstItem]:all_?.strokes.slice(0,all_.strokes.findIndex(stroke=>isHighlighter(stroke)||isFirstStroke(stroke))<0?undefined:all_.strokes.findIndex(stroke=>isHighlighter(stroke)||isFirstStroke(stroke)))??[];
    const frozen=all_&&{...all_,strokes:batch};
    const first=frozen?.strokes.length===1&&isFirstStroke(frozen.strokes[0]);
    const markerPage=marker&&latest?.projection.pages.find(page=>page.id===marker.pageId);
    if(!frozen?.strokes.length||latest?.noteId!==current.id||!(marker?latest.projection.capabilities.highlighter===true&&markerPage?.ink?.highlighter===true:first?latest.projection.capabilities.firstStroke===true:latest.projection.capabilities.appendInk)||current.syncState==='pending')return;
    if(!first&&frozen.strokes.some(isFirstStroke)){setError('Boş sayfadaki ilk çizgi diğer çizgilerle birlikte kaydedilemiyor. Fazla çizgileri geri alın.');return;}
    if(frozen.revision!==current.revision){setError('Bu not çizim sırasında değişti. Çizimleriniz bu pencerede korunuyor; eski sürümün üzerine kaydedilmedi.');return;}
    let releaseDraft!:()=>void;const ready=new Promise<void>(resolve=>{releaseDraft=resolve;});batchReady.current={id:current.id,ready};
    saveLock.current=true;setSaving(current.id);setError('');
    try{
      const templates=new Map(latest.projection.pages.flatMap(page=>page.strokes.map(stroke=>[stroke.id,stroke] as const)));
      const {note:updated}=await serializeDraft(current.id,async()=>{
        await persistDraft(current.id,all_);releaseDraft();
        let result:{note:Note};
        try{result=await api<{note:Note}>(notePath(current.id)+'/ink','PUT',{expectedRevision:frozen.revision,draftStrokeIds:frozen.strokes.map(stroke=>stroke.id),strokes:frozen.strokes.map(({pageId,sourceStrokeId,points,colorARGB,size,kind})=>{if(kind==='highlighter')return {tool:'highlighter',pageId,points,colorARGB:colorARGB>>>0,size};if(!sourceStrokeId)return {pageId,points,colorARGB:colorARGB>>>0,size};const template=templates.get(sourceStrokeId);return template&&(template.colorARGB>>>0)===(colorARGB>>>0)&&template.size===size?{pageId,sourceStrokeId,points}:{pageId,sourceStrokeId,points,colorARGB:colorARGB>>>0,size};})});}
        catch(error){if(!(error instanceof ApiError&&error.status>=400&&error.status<500)){uncertainTransfer.current.add(current.id);setUncertainVersion(v=>v+1);}throw error;}
        const owned=transferred.current.get(current.id)??new Set<string>();for(const stroke of frozen.strokes)owned.add(stroke.id);transferred.current.set(current.id,owned);
        return result;
      });
      // Strokes not in this write (highlighters waiting their turn) stay in the
      // draft; it moves to the new revision once that projection arrives.
      const sent=new Set(frozen.strokes.map(stroke=>stroke.id));
      setDrafts(previous=>{const next={...previous};const rest=(previous[current.id]?.strokes??[]).filter(stroke=>!sent.has(stroke.id));if(rest.length)next[current.id]={...previous[current.id],strokes:rest};else delete next[current.id];return next;});
      // History stays: these draws become queued Cloud strokes (savedDraw).
      setSavedBatches(previous=>({...previous,[current.id]:{revision:updated.revision,clientIds:frozen.strokes.map(stroke=>stroke.id),strokes:frozen.strokes,...(first?{firstPageId:frozen.strokes[0].pageId}:{})}}));
      setFutures(previous=>({...previous,[current.id]:(previous[current.id]??[]).filter(item=>item.kind!=='erase'||!item.drafts.length)}));
      waitingSince.current=null;
      // Do not replace the projection or unmount a pointer gesture in flight.
      if(captureLock.current)pendingAccept.current=()=>accept(updated);else accept(updated);
    }catch(failure){
      if(failure instanceof ApiError&&failure.status===409&&failure.code==='sync_pending'&&!uncertainTransfer.current.has(current.id)){
        const since=waitingSince.current??=Date.now();
        if(Date.now()-since<10*60*1000){window.setTimeout(()=>setWaitTick(t=>t+1),2000);return;}
      }
      waitingSince.current=null;
      setError(uncertainTransfer.current.has(current.id)?'Çizimin durumunu doğrulamak için uygulamayı yeniden açın.':'Çizimler kaydedilemedi. Taslağınız bu pencerede korunuyor.');}
    finally{releaseDraft();if(batchReady.current?.ready===ready)batchReady.current=null;saveLock.current=false;setSaving(null);}
  };
  // Erasures are saved separately from new strokes (one queued job each).
  const eraseGroups=(target:InkProjection,ids:string[]):InkEraseGroup[]|null=>{
    const groups=new Map<string,InkEraseGroup>();
    for(const id of ids){
      const page=target.pages.find(item=>item.strokes.some(stroke=>stroke.id===id));const stroke=page?.strokes.find(item=>item.id===id);
      if(!page||!stroke?.layerId||stroke.erasable!==true)return null;
      const key=page.id+'|'+stroke.layerId;const group=groups.get(key)??{pageId:page.id,layerId:stroke.layerId,strokeIds:[]};group.strokeIds.push(id);groups.set(key,group);
    }
    for(const group of groups.values()){const page=target.pages.find(item=>item.id===group.pageId)!;if(page.strokes.length<=group.strokeIds.filter(id=>page.strokes.some(stroke=>stroke.id===id)).length)return null;}
    return [...groups.values()];
  };
  const saveErase=async()=>{
    const {note:current,erasures:all,loaded:latest,onSaved:accept,options:{onSaveErase}}=active.current;
    if(!current||!onSaveErase||saveLock.current||captureLock.current)return;
    const frozen=all[current.id];
    if(!frozen?.ids.length||latest?.noteId!==current.id||latest.projection.capabilities.eraseInk!==true||current.syncState==='pending')return;
    if(frozen.revision!==current.revision){setError('Bu not silme sırasında değişti. Silinecek çizgiler kaydedilmedi; işaretler korunuyor.');return;}
    const groups=eraseGroups(latest.projection,frozen.ids);
    if(!groups){setError('İşaretli çizgilerden bazıları artık silinemiyor. İşaretler korunuyor.');return;}
    saveLock.current=true;setSaving(current.id);setError('');
    try{
      const updated=await onSaveErase({noteId:current.id,expectedRevision:frozen.revision,erase:groups});
      setErasures(previous=>{const next={...previous};delete next[current.id];return next;});
      // A saved Cloud delete cannot be undone; older entries stay behind it.
      setHistories(previous=>({...previous,[current.id]:(previous[current.id]??[]).map((item):InkAction=>(item.kind==='erase'&&item.saved.length)||(item.kind==='savedDraw'&&item.ids.some(id=>frozen.ids.includes(id)))?{kind:'savedErase'}:item)}));
      setFutures(previous=>({...previous,[current.id]:(previous[current.id]??[]).filter(item=>item.kind!=='savedDraw'||!item.ids.some(id=>frozen.ids.includes(id)))}));
      accept(updated);
    }catch{setError('Silinen çizgiler kaydedilemedi. İşaretler bu pencerede korunuyor.');}
    finally{saveLock.current=false;setSaving(null);}
  };
  // Lasso moves: one queued job; one Cloud write per page layer.
  const saveMove=async()=>{
    const {note:current,moves:all,loaded:latest,onSaved:accept,options:{onSaveMove}}=active.current;
    if(!current||saveLock.current||captureLock.current)return;
    const frozen=all[current.id];
    if(!frozen||latest?.noteId!==current.id||latest.projection.capabilities.eraseInk!==true||current.syncState!=='synced')return;
    if(frozen.revision!==current.revision){setError('Bu not taşıma sırasında değişti. Taşıma kaydedilmedi; işaret korunuyor.');return;}
    const page=latest.projection.pages.find(item=>item.id===frozen.pageId);
    const layers=new Map<string,string[]>();
    for(const id of frozen.ids){const stroke=page?.strokes.find(item=>item.id===id);if(!stroke?.layerId||stroke.erasable!==true){setError('Taşınan çizgilerden bazıları artık taşınamıyor. İşaret korunuyor.');return;}layers.set(stroke.layerId,[...(layers.get(stroke.layerId)??[]),id]);}
    const request:InkMoveRequest={noteId:current.id,expectedRevision:frozen.revision,move:[...layers].map(([layerId,strokeIds])=>({pageId:frozen.pageId,layerId,strokeIds,dx:frozen.dx,dy:frozen.dy}))};
    saveLock.current=true;setSaving(current.id);setError('');
    try{
      const updated=onSaveMove?await onSaveMove(request):(await api<{note:Note}>(notePath(request.noteId)+'/ink-move','PUT',{expectedRevision:request.expectedRevision,move:request.move})).note;
      setMoves(previous=>{const next={...previous};delete next[current.id];return next;});
      accept(updated);
    }catch(reason){setError(`Taşıma kaydedilemedi${reason instanceof Error&&reason.message?': '+reason.message:'.'} Çizgiler yerinde gösteriliyor; geri alabilirsiniz.`);}
    finally{saveLock.current=false;setSaving(null);}
  };
  // Samsung clients have no explicit save: a move mark persists shortly after
  // the last drag once nothing else of this note waits to be saved.
  useEffect(()=>{
    if(!note||!moveMark||capturing||saving||error||note.syncState!=='synced'||drafts[note.id]?.strokes.length||erasures[note.id]?.ids.length||moveMark.revision!==projection?.revision)return;
    const timer=setTimeout(()=>void saveMove(),1500);return()=>clearTimeout(timer);
  },[note?.id,note?.syncState,moveMark,capturing,saving,error,drafts,erasures,projection?.revision]);
  return {projection,draft,erasure,moveMark,phase,error,waitTick,drawing,setDrawing,tool,setTool,capturing,setCapturing,noteSyncState:note?.syncState,
    hasDraft:(id:string)=>!!drafts[id]?.strokes.length||!!erasures[id]?.ids.length||!!moves[id],
    sourceId:note?sources[note.id]??'':'',chooseSource,penStyle:note?penStyles[note.id]??{}:{},setPenStyle,highlighterStyle,setHighlighterStyle,
    undo,redo,canUndo:!undoReason,canRedo:!redoReason,undoReason,redoReason,
    captureInProgress:()=>captureLock.current,
    saving:!!note&&saving===note.id,anySaving:!!saving,restoring:!!note&&(loadingDraft.current.has(note.id)||restoring.current.has(note.id)),transferUncertain:!!note&&uncertainTransfer.current.has(note.id),
    dirtyIds:[...new Set([...Object.keys(drafts).filter(id=>drafts[id].strokes.length>0),...Object.keys(erasures).filter(id=>erasures[id].ids.length>0),...Object.keys(moves)])],
    eraseDirty:erasedNow.length>0,canSaveErase:!!options.onSaveErase&&!!projection?.capabilities.eraseInk&&erasedNow.length>0&&(!erasure||erasure.revision===projection?.revision),
    moveDirty:!!moveMark,saveMove,
    geometry:(pageId:string)=>geometry[pageId],loadGeometry,
    pen:(strokeId:string):InkPenTemplate|undefined=>pens&&pens.noteId===note?.id?pens.pens[strokeId]:undefined,
    change,draw,erase,move,save,saveErase};
}
export type InkController=ReturnType<typeof useInkDrafts>;
export interface InkStyle {id:string;kind:number|string;colorARGB:number;size:number}
// Distinct pen styles on a page, in first-use order. Each entry points at the
// first stroke carrying it; that stroke is the template a new line copies.
export function inkStyles(page:InkPage|undefined):InkStyle[] {
  const seen=new Set<string>(),styles:InkStyle[]=[];
  for(const stroke of page?.strokes??[]){if(stroke.template===false)continue;const key=`${stroke.kind}|${stroke.colorARGB>>>0}|${stroke.size}`;if(seen.has(key))continue;seen.add(key);styles.push({id:stroke.id,kind:stroke.kind,colorARGB:stroke.colorARGB,size:stroke.size});}
  return styles;
}
export function inkSource(page:InkPage|undefined,chosen:string){const templates=page?.strokes.filter(item=>item.template!==false)??[];return templates.find(item=>item.id===chosen)||templates[0];}
// How a new line can start on a page: copying a saved stroke's pen
// ('template') or, on a stroke-free page, as its first stroke ('first').
// Both the note-level capability and the page's own writer facts must allow it.
export function pageDrawMode(projection:InkProjection|null,page:InkPage|undefined,pendingAllowed=true):'template'|'first'|null{
  if(!projection||!page)return null;
  if(page.strokes.some(stroke=>stroke.template!==false))return (projection.capabilities.appendInk===true||(pendingAllowed&&projection.pendingStrokes.length>0))&&page.ink?.appendInk!==false?'template':null;
  if(pendingAllowed&&!page.strokes.length&&projection.pendingStrokes.some(stroke=>stroke.pageId===page.id&&!stroke.sourceStrokeId&&!isHighlighter(stroke)))return projection.optimistic?'first':'template';
  return (projection.capabilities.firstStroke===true||(pendingAllowed&&projection.pendingStrokes.length>0))&&page.ink?.firstStroke===true&&!page.strokes.length?'first':null;
}
// A highlighter line can start on a page whose writer facts allow recipe 5;
// while our own batch syncs, the page facts of the queued projection count.
export function pageHighlighterMode(projection:InkProjection|null,page:InkPage|undefined,pendingAllowed=true){
  if(!projection||!page||page.ink?.highlighter!==true)return false;
  return projection.capabilities.highlighter===true||(pendingAllowed&&projection.pendingStrokes.length>0);
}
// A first stroke is saved alone (recipe 4); until then no other line starts.
function draftAllows(controller:InkController,mode:'template'|'first'|null){return controller.tool==='highlighter'||mode==='template'||mode==='first';}
// Drawing stays possible while our own previous batch is still syncing;
// saving (canAppendInk) waits until the note is synced again.
// The page image does not gate ink: page identity, size and revision come
// from the projection, so a stroke drawn over a page whose image is still
// loading (or missing) is placed and saved exactly the same. `_complete` stays
// in the signatures for call-site stability only.
export function canDrawInk(controller:InkController,pageIndex:number,_complete:boolean,blocked:boolean){const {projection,draft}=controller;const page=projection?.pages.find(item=>item.index===pageIndex);return (controller.tool==='highlighter'?pageHighlighterMode(projection,page):true)&&draftAllows(controller,controller.tool==='highlighter'?'template':pageDrawMode(projection,page))&&!!page&&!blocked&&(!draft||draft.revision===projection!.revision||(projection!.optimistic===true&&draft.revision===projection!.revision-1));}
export function canAppendInk(controller:InkController,pageIndex:number,_complete:boolean,blocked:boolean){
  const {projection,draft}=controller;if(!projection||projection.optimistic||blocked||controller.saving||controller.restoring||controller.transferUncertain||(draft&&draft.revision!==projection.revision))return false;
  const first=draft?.strokes[0];
  if(first&&isHighlighter(first))return pageHighlighterMode(projection,projection.pages.find(page=>page.id===first.pageId),false);
  if(first&&isFirstStroke(first))return pageDrawMode(projection,projection.pages.find(page=>page.id===first.pageId),false)==='first';
  return pageDrawMode(projection,projection.pages.find(page=>first?page.id===first.pageId:page.index===pageIndex),false)==='template';
}
// Stroke eraser: needs the writer's erase capability for the note and page
// (or our own unsaved strokes on the page).
export function canEraseInk(controller:InkController,pageIndex:number,_complete:boolean,blocked:boolean){const {projection,draft,erasure}=controller;const page=projection?.pages.find(item=>item.index===pageIndex);if(!page||blocked||controller.saving||controller.transferUncertain)return false;const own=!!draft?.strokes.some(stroke=>stroke.pageId===page.id);return (own&&(!draft||draft.revision===projection!.revision))||(projection!.capabilities.eraseInk===true&&page.ink?.eraseInk!==false&&page.strokes.some(stroke=>stroke.erasable===true)&&(!erasure||erasure.revision===projection!.revision));}
// Lasso selection works on the same strokes the eraser may change.
export function canSelectInk(controller:InkController,pageIndex:number,complete:boolean,blocked:boolean){const {projection,moveMark}=controller;return canEraseInk(controller,pageIndex,complete,blocked)&&(!moveMark||moveMark.revision===projection?.revision);}
// Short Turkish reason a rail tool is unavailable on the current page.
// Prefers the writer's own per-page / per-note reason from the projection.
export function inkToolReason(controller:InkController,pageIndex:number,_complete:boolean,blocked:boolean,tool:InkTool,pageMode:boolean):string {
  const {projection,phase,draft}=controller;
  if(phase==='idle')return pageMode?'Kalem bu notta kullanılamıyor.':'Yazı notlarında kalem şimdilik kullanılamıyor.';
  if(phase==='loading')return 'Çizimler yükleniyor…';
  if(phase==='unavailable'||!projection)return 'Bu notun çizimleri okunamadı.';
  const page=projection.pages.find(item=>item.index===pageIndex);
  if(!page)return projection.reason||'Bu sayfaya şimdilik çizim eklenemiyor.';
  if(draft&&draft.revision!==projection.revision)return 'Not başka bir yerde değişti; yeni çizgileriniz bu pencerede korunuyor.';
  if(controller.saving&&tool!=='pen'&&tool!=='highlighter')return 'Çizimler kaydediliyor.';
  if(blocked)return 'Notun önceki değişikliği eşitleniyor; birazdan çizebilirsiniz.';
  const reason=page.ink?.reason||projection.reason||'';
  if(tool==='highlighter'){
    if((draft?.strokes??[]).some(isFirstStroke))return 'Boş sayfadaki ilk çizgi kaydediliyor; eşitlenince vurgulayabilirsiniz.';
    return pageHighlighterMode(projection,page)?'':reason||'Bu sayfaya fosforlu kalem çizgisi eklenemiyor.';
  }
  if(tool==='pen'){
    const mode=pageDrawMode(projection,page),strokes=(draft?.strokes??[]).filter(stroke=>!isHighlighter(stroke));
    if(strokes.some(isFirstStroke))return 'Boş sayfadaki ilk çizgi kaydediliyor; eşitlenince çizmeye devam edebilirsiniz.';
    if(mode==='first'&&strokes.length)return 'Boş sayfaya ilk çizgi için önce mevcut çizimler kaydedilmeli.';
    return mode?'':reason||(page.strokes.some(stroke=>stroke.template!==false)?'Bu sayfaya şu anda çizgi eklenemiyor.':'Bu sayfaya şimdilik çizim eklenemiyor.');
  }
  if(projection.capabilities.eraseInk!==true||page.ink?.eraseInk===false)return reason||'Bu nottaki çizgiler şimdilik düzenlenemiyor.';
  if(!page.strokes.some(stroke=>stroke.erasable===true))return page.ink?.reason||'Bu sayfada düzenlenebilir çizgi yok.';
  if(tool==='select'&&controller.moveMark&&controller.moveMark.revision!==projection.revision)return 'Not değişti; taşıma işareti kaydedilmedi.';
  return reason;
}
export function inkColor(value:number){const unsigned=value>>>0;return {color:`#${(unsigned&0xffffff).toString(16).padStart(6,'0')}`,opacity:(unsigned>>>24)/255};}
function Stroke({stroke}:{stroke:InkStroke}){const {color,opacity}=inkColor(stroke.colorARGB);if(isHighlighter(stroke))return <HighlighterStroke stroke={stroke} color={color} opacity={opacity}/>;if(stroke.points.length===1)return <circle cx={stroke.points[0].x} cy={stroke.points[0].y} r={stroke.size/2} fill={color} fillOpacity={opacity}/>;return <polyline points={stroke.points.map(point=>`${point.x},${point.y}`).join(' ')} fill="none" stroke={color} strokeOpacity={opacity} strokeWidth={stroke.size} strokeLinecap="round" strokeLinejoin="round"/>;}
// Draft highlighter: one flat translucent band (Samsung applies the alpha once
// to the stamp union, in a darken batch); the tip is a rounded rectangle, so
// the band uses square-ish joins. Approximation, not the native GPU result.
function HighlighterStroke({stroke,color,opacity}:{stroke:InkStroke;color:string;opacity:number}){const style={mixBlendMode:'darken' as const};if(stroke.points.length===1){const {x,y}=stroke.points[0];return <rect className="ink-highlighter" style={style} x={x-stroke.size*.385} y={y-stroke.size/2} width={stroke.size*.77} height={stroke.size} rx={stroke.size*.19} fill={color} fillOpacity={opacity}/>;}return <polyline className="ink-highlighter" style={style} points={stroke.points.map(point=>`${point.x},${point.y}`).join(' ')} fill="none" stroke={color} strokeOpacity={opacity} strokeWidth={stroke.size} strokeLinecap="round" strokeLinejoin="round"/>;}
// Saved strokes marked for deletion are painted over in the page colour until
// the Cloud write and new page image arrive (an approximation of the result).
function Mask({points,size,className}:{points:[number,number][];size:number;className:string}){if(points.length===1)return <circle className={className} cx={points[0][0]} cy={points[0][1]} r={size/2+1.5}/>;return <polyline className={className} points={points.map(point=>point.join(',')).join(' ')} fill="none" strokeWidth={size+3} strokeLinecap="round" strokeLinejoin="round"/>;}
// A saved stroke drawn at its moved position (simplified geometry, own colour).
function MovedStroke({points,size,colorARGB,dx,dy}:{points:[number,number][];size:number;colorARGB:number;dx:number;dy:number}){const {color,opacity}=inkColor(colorARGB);if(points.length===1)return <circle className="ink-moved" cx={points[0][0]+dx} cy={points[0][1]+dy} r={size/2} fill={color} fillOpacity={opacity}/>;return <polyline className="ink-moved" points={points.map(([x,y])=>`${x+dx},${y+dy}`).join(' ')} fill="none" stroke={color} strokeOpacity={opacity} strokeWidth={size} strokeLinecap="round" strokeLinejoin="round"/>;}
// A finished pen stroke drawn by the live renderer (device pixels, placed in
// CSS pixels over the page). Layout effect: painted in the same frame it mounts.
type PixelBox={w:number;h:number;x:number;y:number;dpr:number};
function RasterCanvas({id,raster,pageWidth,pageHeight,dx,dy,faded,pixel,snap}:{id:string;raster:LiveRaster;pageWidth:number;pageHeight:number;dx:number;dy:number;faded:boolean;pixel:PixelBox;snap:boolean}){
  // Page-relative layout scales immediately with the image, including before
  // ResizeObserver reports the new size and the next raster is ready.
  const x=(raster.x/raster.scale+dx)/pageWidth*100,y=(raster.y/raster.scale+dy)/pageHeight*100;
  const width=raster.width/raster.scale/pageWidth*100,height=raster.height/raster.scale/pageHeight*100;
  const adjusted=(percent:number,extent:number,origin:number)=>{const raw=percent*extent/100;return snap?(snapInkOffset(origin,raw,pixel.dpr)??raw)-raw:0;};
  const l=adjusted(x,pixel.w,pixel.x),t=adjusted(y,pixel.h,pixel.y),r=adjusted(x+width,pixel.w,pixel.x),b=adjusted(y+height,pixel.h,pixel.y);
  const ref=useRef<HTMLCanvasElement>(null);
  useLayoutEffect(()=>{const canvas=ref.current;if(!canvas)return;canvas.width=raster.width;canvas.height=raster.height;canvas.getContext('2d')?.putImageData(raster.image,0,0);},[raster]);
  return <canvas ref={ref} className={`ink-raster${faded?' ink-erase-hit':''}`} data-ink-stroke={id} aria-hidden="true" style={{left:`calc(${x}% + ${l}px)`,top:`calc(${y}% + ${t}px)`,width:`calc(${width}% + ${r-l}px)`,height:`calc(${height}% + ${b-t}px)`}}/>;
}
type InkTileImage={column:number;row:number;url:string;image:HTMLImageElement};
type InkTileFrame={noteId:string;pageId:string;base:string;token:string;bucket:number;width:number;height:number;tiles:InkTileImage[];textless:boolean};
type TileDraft={raster:LiveRaster;dx:number;dy:number;faded:boolean};
function InkTileCanvas({tile,frame,pixel,drafts}:{tile:InkTileImage;frame:InkTileFrame;pixel:PixelBox|null;drafts:TileDraft[]}){
  const ref=useRef<HTMLCanvasElement>(null);
  useLayoutEffect(()=>{
    const canvas=ref.current;if(!canvas)return;canvas.width=512;canvas.height=512;
    const context=canvas.getContext('2d');if(!context)return;context.drawImage(tile.image,0,0);
    for(const {raster,dx,dy,faded} of drafts){
      const ratio=frame.bucket/raster.scale,x=(raster.x+raster.scale*dx)*ratio-tile.column*512,y=(raster.y+raster.scale*dy)*ratio-tile.row*512;
      const width=raster.width*ratio,height=raster.height*ratio;if(x>=512||y>=512||x+width<=0||y+height<=0)continue;
      const source=document.createElement('canvas');source.width=raster.width;source.height=raster.height;source.getContext('2d')?.putImageData(raster.image,0,0);
      context.save();context.globalAlpha=faded ? .35 : 1;context.drawImage(source,x,y,width,height);context.restore();
    }
  },[tile,frame.bucket,drafts]);
  const x=tile.column*512/frame.bucket/frame.width*100,y=tile.row*512/frame.bucket/frame.height*100,w=512/frame.bucket/frame.width*100,h=512/frame.bucket/frame.height*100;
  const delta=(percent:number,extent:number,origin:number)=>{if(!pixel)return 0;const raw=percent*extent/100;return (snapInkOffset(origin,raw,pixel.dpr)??raw)-raw;};
  const l=delta(x,pixel?.w??0,pixel?.x??0),t=delta(y,pixel?.h??0,pixel?.y??0),r=delta(x+w,pixel?.w??0,pixel?.x??0),b=delta(y+h,pixel?.h??0,pixel?.y??0);
  return <canvas ref={ref} className="ink-raster ink-tile" aria-hidden="true" data-tile={`${tile.column},${tile.row}`} style={{left:`calc(${x}% + ${l}px)`,top:`calc(${y}% + ${t}px)`,width:`calc(${w}% + ${r-l}px)`,height:`calc(${h}% + ${b-t}px)`}}/>;
}
const frameUrls=(frame:InkTileFrame|null)=>frame?[frame.base,...frame.tiles.map(tile=>tile.url)]:[];
// The existing bitmap is the fallback until the entire visible tile set and
// its base have decoded. A zoom retains the previous set until the new one is
// committed in a single animation frame.
function useInkTiles(box:React.RefObject<HTMLDivElement|null>,page:InkPage|undefined,phase:string,syncState:string|undefined,pendingCount:number,allowReplace:boolean,cover:InkStroke[]){
  const context=useContext(InkPageContext);
  const [frame,setFrame]=useState<InkTileFrame|null>(null);
  const [tileRetry,setTileRetry]=useState(0);
  const frameRef=useRef<InkTileFrame|null>(null);frameRef.current=frame;
  const [view,setView]=useState<{bucket:number;startX:number;endX:number;startY:number;endY:number;visibleX0:number;visibleX1:number;visibleY0:number;visibleY1:number}|null>(null);
  const currentUrls=useRef<string[]>([]);
  useLayoutEffect(()=>{if(frameRef.current&&(frameRef.current.noteId!==context?.noteId||frameRef.current.pageId!==page?.id))setFrame(null);},[context?.noteId,page?.id]);
  useLayoutEffect(()=>{const next=frameUrls(frame);for(const url of currentUrls.current)if(!next.includes(url))URL.revokeObjectURL(url);currentUrls.current=next;},[frame]);
  useEffect(()=>()=>{for(const url of currentUrls.current)URL.revokeObjectURL(url);currentUrls.current=[];},[]);
  const eligible=!!context&&!context.suppressTiles&&context.visible&&!!page&&phase==='ready'&&(context.textless||['ink-reconstructed','page-reconstructed','pdf-with-notes'].includes(context.page.previewKind??''));
  useLayoutEffect(()=>{
    const element=box.current;if(!eligible||!element||!page){setView(null);return;}
    const scroll=element.closest('.canvas-scroll');if(!scroll)return;let raf=0;
    const measure=()=>{raf=0;const rect=element.getBoundingClientRect(),viewport=scroll.getBoundingClientRect();
      const bucket=inkTileBucket(rect.width*(window.devicePixelRatio||1)/page.width);if(!bucket){setView(null);return;}
      const all=visibleInkTiles(rect,viewport,page,bucket,1),visible=visibleInkTiles(rect,viewport,page,bucket,0);if(!all||!visible){setView(null);return;}
      const next={bucket,startX:all.columnStart,endX:all.columnEnd,startY:all.rowStart,endY:all.rowEnd,visibleX0:visible.columnStart,visibleX1:visible.columnEnd,visibleY0:visible.rowStart,visibleY1:visible.rowEnd};
      setView(old=>old&&Object.keys(next).every(key=>old[key as keyof typeof next]===next[key as keyof typeof next])?old:next);
    };
    const schedule=()=>{if(!raf)raf=requestAnimationFrame(measure);};
    const observer=new ResizeObserver(schedule);observer.observe(element);observer.observe(scroll);scroll.addEventListener('scroll',schedule,{passive:true});measure();
    return()=>{observer.disconnect();scroll.removeEventListener('scroll',schedule);if(raf)cancelAnimationFrame(raf);};
  },[eligible,page?.id,page?.width,page?.height]);
  useEffect(()=>{
    if(!context||!view||!page||!eligible)return;
    const abort=new AbortController();const created:string[]=[];let committed=false,raf=0,retry:ReturnType<typeof setTimeout>|undefined;
    const pause=()=>new Promise<void>((resolve,reject)=>{const onAbort=()=>{clearTimeout(timer);reject(new DOMException('Aborted','AbortError'));};const timer=setTimeout(()=>{abort.signal.removeEventListener('abort',onAbort);resolve();},100);abort.signal.addEventListener('abort',onAbort,{once:true});});
    const fetchImage=async(path:string)=>{
      for(;;){const response=await request(path,{signal:abort.signal});if(response.status===202){await pause();continue;}
        if(response.headers.get('content-type')?.split(';')[0].trim()!=='image/png')throw Error('invalid tile');
        const blob=await response.blob();if(abort.signal.aborted)throw new DOMException('Aborted','AbortError');
        const url=URL.createObjectURL(blob);created.push(url);const image=new Image();image.src=url;await image.decode();
        return {url,image,token:response.headers.get('x-ink-snapshot'),supported:response.headers.get('x-ink-tiles')};
      }
    };
    const run=async()=>{
      const prefix=notePath(context.noteId)+`/preview?page=${page.index}`+(context.revision===undefined?'':`&revision=${context.revision}`);
      const base=await fetchImage(prefix+(context.textless?'&variant=notextbase':'&variant=noink'));if(base.supported!=='1'||!base.token)throw Error('unsupported layer');
      // Never substitute a different snapshot while queued strokes are still
      // represented by the overlay. The previous frame remains visible.
      if(frameRef.current?.noteId===context.noteId&&frameRef.current.pageId===page.id&&(pendingCount||!allowReplace)&&frameRef.current.token!==base.token)return;
      const coords:{column:number;row:number;visible:boolean}[]=[];
      for(let row=view.startY;row<=view.endY;row++)for(let column=view.startX;column<=view.endX;column++)coords.push({column,row,visible:column>=view.visibleX0&&column<=view.visibleX1&&row>=view.visibleY0&&row<=view.visibleY1});
      const existing=new Set(coords.map(item=>`${item.column},${item.row}`));
      for(const stroke of cover){if(stroke.pageId!==page.id||!stroke.points.length)continue;const pad=stroke.size/2+4;
        const xs=stroke.points.map(point=>point.x),ys=stroke.points.map(point=>point.y);
        const x0=Math.max(0,Math.floor((Math.min(...xs)-pad)*view.bucket/512)),x1=Math.min(Math.ceil(page.width*view.bucket/512)-1,Math.floor((Math.max(...xs)+pad)*view.bucket/512));
        const y0=Math.max(0,Math.floor((Math.min(...ys)-pad)*view.bucket/512)),y1=Math.min(Math.ceil(page.height*view.bucket/512)-1,Math.floor((Math.max(...ys)+pad)*view.bucket/512));
        for(let row=y0;row<=y1;row++)for(let column=x0;column<=x1;column++){const key=`${column},${row}`;if(!existing.has(key)){existing.add(key);coords.push({column,row,visible:false});}}
      }
      coords.sort((a,b)=>Number(b.visible)-Number(a.visible));
      const tiles:InkTileImage[]=[];
      for(const {column,row} of coords){const tile=await fetchImage(prefix+`&layer=${context.textless?'textink':'ink'}&scale=${view.bucket}&tile=${column},${row}&snapshot=${base.token}`);if(tile.token!==base.token)throw Error('snapshot changed');tiles.push({column,row,url:tile.url,image:tile.image});}
      if(abort.signal.aborted)return;
      const next={noteId:context.noteId,pageId:page.id,base:base.url,token:base.token,bucket:view.bucket,width:page.width,height:page.height,tiles,textless:context.textless};
      raf=requestAnimationFrame(()=>{if(abort.signal.aborted)return;committed=true;setFrame(next);});
    };
    void run().catch(error=>{
      if(abort.signal.aborted)return;
      if(error?.message==='unsupported layer'||(error instanceof ApiError&&[400,401,403,404].includes(error.status))){if(context.textless)context.onTextLayerFailed?.(page.index);return;}
      // A source can change between its base and tile requests. Retry with a
      // fresh token while retaining the old, decoded frame.
      retry=setTimeout(()=>setTileRetry(value=>value+1),error instanceof ApiError&&error.status===409?250:2000);
    }).finally(()=>{if(!committed&&!raf){for(const url of created)URL.revokeObjectURL(url);}});
    return()=>{abort.abort();if(raf)cancelAnimationFrame(raf);if(retry)clearTimeout(retry);if(!committed)for(const url of created)URL.revokeObjectURL(url);};
  },[eligible,context?.noteId,context?.revision,context?.textless,page?.id,page?.width,page?.height,view,syncState,pendingCount,allowReplace,tileRetry,cover.map(stroke=>stroke.id).join('|')]);
  useEffect(()=>{if(context?.textless&&frame?.textless&&frame.noteId===context.noteId&&frame.pageId===page?.id)context.onTextLayerReady?.(page.index);},[context?.textless,context?.noteId,context?.onTextLayerReady,frame,page?.id,page?.index]);
  return eligible&&frame?.noteId===context?.noteId&&frame?.pageId===page?.id&&frame.textless===context?.textless?frame:null;
}
// Optional probe for headless timing (tests set window.__inkLiveProbe = []).
type LiveProbe={kind:'tip'|'exact';event:number;painted:number;points:number}[];
const ERASER_CSS_RADIUS=7;
interface Selection {pageId:string;saved:string[];drafts:string[]}
// One page of a note: saved page image plus its ink layer. Pen mode is shared
// by all pages; only the page the user is reading shows notices.
export function InkEditor({controller,pageIndex,current:isCurrent=true,baselineComplete,blocked,imageUrl,onImageError,maskColor='#ffffff'}:{controller:InkController;pageIndex:number;current?:boolean;baselineComplete:boolean;blocked:boolean;imageUrl:string|null;onImageError:()=>void;maskColor?:string;readingMode?:boolean}){
  const {projection,draft,erasure,moveMark,phase,drawing,tool,setCapturing}=controller;
  const page=projection?.pages.find(item=>item.index===pageIndex);
  const [current,setCurrent]=useState<InkStroke|null>(null);const [,setNotice]=useState('');
  const [hits,setHits]=useState<{saved:string[];drafts:string[]}|null>(null);
  const [selection,setSelection]=useState<Selection|null>(null);
  const [lasso,setLasso]=useState<[number,number][]|null>(null);const [drag,setDrag]=useState<[number,number]|null>(null);
  const canvas=useRef<SVGSVGElement>(null);const gesture=useRef<{pointer:number;started:number;stroke:InkStroke}|null>(null);
  const eraseGesture=useRef<{pointer:number;path:[number,number][];saved:Set<string>;drafts:Set<string>;blocked:boolean}|null>(null);
  const selectGesture=useRef<{pointer:number;mode:'lasso'|'drag';start:[number,number];path:[number,number][];offset:[number,number]}|null>(null);
  const erasing=tool==='eraser',selecting=tool==='select';
  const available=selecting?canSelectInk(controller,pageIndex,baselineComplete,blocked):erasing?canEraseInk(controller,pageIndex,baselineComplete,blocked):canDrawInk(controller,pageIndex,baselineComplete,blocked);
  const source=inkSource(page,controller.sourceId)??(!projection?.optimistic&&!page?.strokes.length?projection?.pendingStrokes.find(stroke=>stroke.pageId===page?.id&&!stroke.sourceStrokeId&&!isHighlighter(stroke)):undefined);
  const strokes=draft?.strokes??[];const pending=projection?.pendingStrokes??[];
  // Marked here, or saved and waiting for the Cloud write (pendingErase).
  const queued=(projection?.pendingErase??[]).filter(group=>group.pageId===page?.id).flatMap(group=>group.strokeIds);
  const erased=new Set([...(erasure?.ids??[]),...queued]);
  const queuedMoves=queuedOffsets(projection);
  const pageMoves=!!page&&((projection?.pendingMove??[]).some(group=>group.pageId===page.id)||moveMark?.pageId===page.id);
  const geometry=page?controller.geometry(page.id):undefined;
  const latest=useRef({strokes,draw:controller.draw,erase:controller.erase,move:controller.move});latest.current={strokes,draw:controller.draw,erase:controller.erase,move:controller.move};
  // Live pen renderer (WebAssembly). Without it every stroke keeps the SVG preview.
  const live=useInkLive(phase==='ready');
  const pageBox=useRef<HTMLDivElement>(null);const liveCanvas=useRef<HTMLCanvasElement>(null);
  const pagePending=pending.filter(stroke=>stroke.pageId===page?.id);
  const previousPending=useRef<{pageId:string|undefined;token:string|null;strokes:InkStroke[]}>({pageId:undefined,token:null,strokes:[]});
  const [retainedPending,setRetainedPending]=useState<{stroke:InkStroke;token:string}[]>([]);
  const cover=[...previousPending.current.strokes,...retainedPending.map(item=>item.stroke)];
  const allowReplace=cover.every(stroke=>pagePending.some(item=>item.id===stroke.id)||page?.strokes.some(item=>item.id===stroke.id));
  const tileFrame=useInkTiles(pageBox,page,phase,controller.noteSyncState,pagePending.length,allowReplace,cover);
  const pendingSignature=pagePending.map(stroke=>stroke.id).join('|');
  useLayoutEffect(()=>{
    const previous=previousPending.current,token=tileFrame?.token??null;
    const same=(a:InkStroke,b:InkStroke)=>a.pageId===b.pageId&&a.colorARGB===b.colorARGB&&a.size===b.size&&a.points.length===b.points.length&&a.points.every((point,i)=>point.x===b.points[i].x&&point.y===b.points[i].y&&point.pressure===b.points[i].pressure&&point.timeMs===b.points[i].timeMs);
    const removed=previous.pageId===page?.id&&previous.token&&previous.token===token?previous.strokes.filter(stroke=>!pagePending.some(current=>same(stroke,current))):[];
    setRetainedPending(old=>{const kept=old.filter(item=>item.token===token&&!pagePending.some(current=>same(item.stroke,current)));
      for(const stroke of removed)if(!kept.some(item=>same(item.stroke,stroke)))kept.push({stroke,token:previous.token!});
      return kept.length===old.length&&kept.every((item,i)=>item===old[i])?old:kept;});
    previousPending.current={pageId:page?.id,token,strokes:pagePending};
  },[page?.id,pendingSignature,tileFrame?.token]);
  const [pixel,setPixel]=useState<PixelBox|null>(null);
  useLayoutEffect(()=>{
    const element=pageBox.current;if(!element)return;
    const measure=()=>{const rect=element.getBoundingClientRect(),dpr=window.devicePixelRatio||1;setPixel(previous=>previous&&previous.w===rect.width&&previous.h===rect.height&&previous.x===rect.left&&previous.y===rect.top&&previous.dpr===dpr?previous:rect.width&&rect.height?{w:rect.width,h:rect.height,x:rect.left,y:rect.top,dpr}:null);};
    measure();if(typeof ResizeObserver!=='function')return;const observer=new ResizeObserver(measure);observer.observe(element);const scroll=tileFrame?element.closest('.canvas-scroll'):null;scroll?.addEventListener('scroll',measure,{passive:true});return()=>{observer.disconnect();scroll?.removeEventListener('scroll',measure);};
  },[!!page,phase,!!live,!!tileFrame]);
  const scale=pixel&&page?(tileFrame?.bucket??pixel.w*pixel.dpr/page.width):0;
  // The pen the writer will store for this stroke; null keeps the SVG preview.
  const specOf=(stroke:InkStroke):LivePenSpec|null=>{
    if(!page||isHighlighter(stroke)||!Number.isFinite(stroke.size)||stroke.size<=0)return null;
    const base={colorARGB:stroke.colorARGB>>>0,size:stroke.size,pageWidth:page.width,pageHeight:page.height};
    if(isFirstStroke(stroke))return base;
    const template=controller.pen(stroke.sourceStrokeId);
    return template&&template.width===page.width&&template.height===page.height&&template.rendering&&typeof template.rendering==='object'?{...base,rendering:template.rendering}:null;
  };
  // Raster of a finished stroke: null = SVG preview, undefined = being made.
  const [,setRasterVersion]=useState(0);
  const rasterOf=(stroke:InkStroke):LiveRaster|null|undefined=>{
    if(!live||!scale||stroke.points.length<2)return null;const spec=specOf(stroke);if(!spec)return null;
    const value=live.cached(live.key(spec,stroke.points,scale));
    if(value===undefined)void live.ensure(spec,stroke.points,scale).then(()=>setRasterVersion(version=>version+1));
    return value;
  };
  // Pixels on screen at pen-up, kept until the stroke's own raster is ready;
  // after a zoom the previous raster stays (scaled) until the new one arrives.
  const [handoffs,setHandoffs]=useState<Record<string,LiveRaster>>({});
  const shown=useRef(new Map<string,LiveRaster>());
  type Rect=[number,number,number,number];
  const liveGesture=useRef<{spec:LivePenSpec;scale:number;width:number;color:string;opacity:number;predicted:InkPoint[];expire:number;frame:number;busy:boolean;rendered:number;exact:LiveRaster|null;tail:Rect|null;events:number[];exactEvents:number[]}|null>(null);
  const liveContext=()=>liveCanvas.current?.getContext('2d',{desynchronized:true})??null;
  const clearLive=()=>{const g=liveGesture.current;liveGesture.current=null;if(g?.frame)cancelAnimationFrame(g.frame);clearTimeout(g?.expire);const canvas=liveCanvas.current;liveContext()?.clearRect(0,0,canvas?.width??0,canvas?.height??0);};
  const probe=()=>(window as unknown as {__inkLiveProbe?:LiveProbe}).__inkLiveProbe;
  // The part not yet rendered exactly (newest samples plus the predicted tip,
  // which is never stored) is drawn at once as a plain line; the worker's
  // exact raster replaces it, usually within a frame or two.
  const paintTail=(g:NonNullable<typeof liveGesture.current>,points:readonly InkPoint[],withPredicted:boolean)=>{
    const context=liveContext();if(!context)return;
    // Remove the previous plain tail and restore the exact pixels under it.
    if(g.tail){const [tx,ty,tw,th]=g.tail;context.clearRect(tx,ty,tw,th);if(g.exact)context.putImageData(g.exact.image,g.exact.x,g.exact.y,tx-g.exact.x,ty-g.exact.y,tw,th);g.tail=null;}
    const anchor=points[points.length-1];
    const predicted=withPredicted&&anchor?g.predicted.filter(point=>Math.hypot(point.x-anchor.x,point.y-anchor.y)*g.scale>=1):[];
    const tail=[...points.slice(Math.max(0,g.rendered-1)),...predicted];
    if(tail.length<2&&!(g.rendered===0&&tail.length===1))return;
    const s=g.scale,w=Math.max(1,g.width*s);let x0=Infinity,y0=Infinity,x1=-Infinity,y1=-Infinity;
    context.save();context.strokeStyle=g.color;context.fillStyle=g.color;context.globalAlpha=g.opacity;context.lineWidth=w;context.lineCap='round';context.lineJoin='round';context.beginPath();
    tail.forEach((point,i)=>{const x=point.x*s,y=point.y*s;x0=Math.min(x0,x);y0=Math.min(y0,y);x1=Math.max(x1,x);y1=Math.max(y1,y);if(i)context.lineTo(x,y);else context.moveTo(x,y);});
    if(tail.length===1)context.lineTo(x0+0.01,y0);context.stroke();context.restore();
    const pad=w/2+2;g.tail=[Math.floor(x0-pad),Math.floor(y0-pad),Math.ceil(x1-x0+2*pad),Math.ceil(y1-y0+2*pad)];
  };
  const requestExact=(g:NonNullable<typeof liveGesture.current>)=>{
    const active=gesture.current;if(!live||g.busy||!active||active.stroke.points.length<2||active.stroke.points.length===g.rendered)return;
    const points=active.stroke.points.slice(),events=g.events;g.events=[];g.busy=true;
    void live.render(g.spec,points,g.scale).then(raster=>{
      g.busy=false;if(liveGesture.current!==g)return;const context=liveContext();if(!context)return;
      // The exact frame replaces the previous frame and the plain tail.
      if(g.exact)context.clearRect(g.exact.x,g.exact.y,g.exact.width,g.exact.height);if(g.tail)context.clearRect(...g.tail);g.exact=null;g.tail=null;
      if(raster){context.putImageData(raster.image,raster.x,raster.y);g.exact=raster;g.rendered=points.length;}
      const list=probe();if(list){const painted=performance.now();for(const event of events)list.push({kind:'exact',event,painted,points:points.length});}
      paintTail(g,gesture.current?.stroke.points??points,true);requestExact(g);
    });
  };
  const paintLive=()=>{
    const g=liveGesture.current,active=gesture.current;if(!g||!active)return;g.frame=0;
    paintTail(g,active.stroke.points,true);
    const list=probe();if(list){const painted=performance.now();for(const event of g.exactEvents)list.push({kind:'tip',event,painted,points:active.stroke.points.length});}
    g.exactEvents=[];requestExact(g);
  };
  // A handoff leaves in the commit that mounts the stroke's own raster.
  useEffect(()=>{
    const done=Object.keys(handoffs).filter(id=>{const stroke=strokes.find(item=>item.id===id)??pending.find(item=>item.id===id);return !stroke||rasterOf(stroke)!==undefined;});
    if(done.length)setHandoffs(previous=>{const next={...previous};for(const id of done)delete next[id];return next;});
  });
  const cancelGestures=()=>{if(gesture.current||eraseGesture.current||selectGesture.current)setCapturing(false);clearLive();gesture.current=null;eraseGesture.current=null;selectGesture.current=null;setCurrent(null);setHits(null);setLasso(null);setDrag(null);};
  useEffect(()=>()=>{gesture.current=null;eraseGesture.current=null;selectGesture.current=null;setCapturing(false);},[]);
  useEffect(()=>{cancelGestures();setNotice('');},[projection?.revision,page?.id,tool]);
  useEffect(()=>{if(!selecting||!drawing)setSelection(null);},[selecting,drawing,page?.id]);
  useEffect(()=>{if(!available)cancelGestures();},[available]);
  // Saved-stroke geometry is fetched only when the eraser/lasso is used on
  // this page or marks/queued edits of this page need drawing.
  useEffect(()=>{if(page&&((drawing&&(erasing||selecting)&&available&&projection?.capabilities.eraseInk)||queued.length||erasure?.ids.length||pageMoves))controller.loadGeometry(page.id);},[drawing,erasing,selecting,available,page?.id,projection?.revision,queued.length,erasure?.ids.length,pageMoves]);
  // A selection keeps only strokes that still exist (moves keep their ids).
  const meta=new Map((page?.strokes??[]).map(stroke=>[stroke.id,stroke]));
  const liveSelection=selection&&page&&selection.pageId===page.id?{...selection,saved:selection.saved.filter(id=>meta.has(id)&&!erased.has(id)),drafts:selection.drafts.filter(id=>strokes.some(stroke=>stroke.id===id))}:null;
  const selectedSaved=new Set(liveSelection?.saved??[]),selectedDrafts=new Set(liveSelection?.drafts??[]);
  // Current offset of a saved stroke: queued Cloud move + unsaved mark.
  const savedOffset=(id:string):[number,number]=>{const [qx,qy]=queuedMoves.get(id)??[0,0];const marked=moveMark&&moveMark.pageId===page?.id&&moveMark.ids.includes(id);return [qx+(marked?moveMark!.dx:0),qy+(marked?moveMark!.dy:0)];};
  const selectionBounds=():[number,number,number,number]|null=>{
    if(!page||!liveSelection)return null;let box:[number,number,number,number]|null=null;
    const add=(l:number,t:number,r:number,b:number)=>{box=box?[Math.min(box[0],l),Math.min(box[1],t),Math.max(box[2],r),Math.max(box[3],b)]:[l,t,r,b];};
    for(const id of liveSelection.saved){const bounds=meta.get(id)?.bounds;const [ox,oy]=savedOffset(id);if(bounds)add(bounds[0]+ox,bounds[1]+oy,bounds[2]+ox,bounds[3]+oy);else{const stroke=geometry?.strokes.find(item=>item.id===id);for(const [x,y] of stroke?.points??[])add(x+ox-stroke!.size/2,y+oy-stroke!.size/2,x+ox+stroke!.size/2,y+oy+stroke!.size/2);}}
    for(const stroke of strokes)if(selectedDrafts.has(stroke.id))for(const point of stroke.points)add(point.x-stroke.size/2,point.y-stroke.size/2,point.x+stroke.size/2,point.y+stroke.size/2);
    return box;
  };
  const deleteSelection=()=>{
    if(!liveSelection||!page)return;let saved=[...liveSelection.saved];
    const remaining=page.strokes.filter(stroke=>!erased.has(stroke.id)).length;
    if(saved.length&&saved.length>=remaining){saved=[];setNotice('Sayfadaki son kayıtlı çizgi silinemez.');}
    latest.current.erase(saved,[...liveSelection.drafts]);setSelection(null);
  };
  const deleteRef=useRef(deleteSelection);deleteRef.current=deleteSelection;
  useEffect(()=>{
    if(!liveSelection||!selecting||!drawing)return;
    const onKey=(event:KeyboardEvent)=>{const target=event.target as HTMLElement|null;if(target&&(target.isContentEditable||/^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)))return;if(event.key==='Delete'||event.key==='Backspace'){event.preventDefault();deleteRef.current();}else if(event.key==='Escape')setSelection(null);};
    window.addEventListener('keydown',onKey);return()=>window.removeEventListener('keydown',onKey);
  },[!!liveSelection,selecting,drawing]);
  const finish=(keep:boolean)=>{
    const active=gesture.current,g=liveGesture.current;gesture.current=null;setCapturing(false);setCurrent(null);
    if(g){
      if(g.frame)cancelAnimationFrame(g.frame);clearTimeout(g.expire);liveGesture.current=null;const canvas=liveCanvas.current,context=liveContext();
      if(active&&keep&&active.stroke.points.length>=2&&live&&canvas&&context){
        // What the user sees at pen-up (minus the predicted tip) moves to a
        // handoff canvas in the same frame; the live layer is cleared.
        paintTail(g,active.stroke.points,false);
        const boxes=[g.exact&&[g.exact.x,g.exact.y,g.exact.width,g.exact.height] as Rect,g.tail].filter((box):box is Rect=>!!box);
        if(boxes.length){
          const x0=Math.max(0,Math.min(...boxes.map(b=>b[0]))),y0=Math.max(0,Math.min(...boxes.map(b=>b[1])));
          const x1=Math.min(canvas.width,Math.max(...boxes.map(b=>b[0]+b[2]))),y1=Math.min(canvas.height,Math.max(...boxes.map(b=>b[1]+b[3])));
          if(x1>x0&&y1>y0){
            // Exact frame of every stored sample already on screen: it is the final raster.
            const final=g.rendered===active.stroke.points.length&&!g.tail&&g.exact?g.exact:null;
            const handoff=final??{x:x0,y:y0,width:x1-x0,height:y1-y0,scale:g.scale,image:context.getImageData(x0,y0,x1-x0,y1-y0)};
            const id=active.stroke.id;setHandoffs(previous=>({...previous,[id]:handoff}));
            if(final)live.store(live.key(g.spec,active.stroke.points,g.scale),final);
          }
        }
        void live.ensure(g.spec,active.stroke.points,g.scale).then(()=>setRasterVersion(version=>version+1));
      }
      context?.clearRect(0,0,canvas?.width??0,canvas?.height??0);
    }
    if(active&&keep&&active.stroke.points.length>=1)latest.current.draw(active.stroke);
  };
  const pagePoint=(item:{clientX:number;clientY:number},rect:DOMRect):[number,number]=>[Math.max(0,Math.min(page!.width,(item.clientX-rect.left)*page!.width/rect.width)),Math.max(0,Math.min(page!.height,(item.clientY-rect.top)*page!.height/rect.height))];
  const collect=(event:ReactPointerEvent<SVGSVGElement>)=>{
    const active=gesture.current,element=canvas.current;if(!active||!page||!element||active.pointer!==event.pointerId)return;
    const rect=element.getBoundingClientRect();if(!rect.width||!rect.height)return;
    const events=event.nativeEvent.getCoalescedEvents?.()??[event.nativeEvent];
    for(const item of events.length?events:[event.nativeEvent]){
      if(pointCount(latest.current.strokes)+active.stroke.points.length>=MAX_POINTS){setNotice('Bu kayıtta çizim sınırına ulaşıldı. Önce mevcut çizimleri kaydedin.');finish(true);return;}
      const timeMs=Math.max(0,Math.round(item.timeStamp-active.started));if(timeMs>65535){setNotice('Uzun çizgi tamamlandı. Yeni bir çizgiyle devam edebilirsiniz.');finish(true);return;}
      const [x,y]=pagePoint(item,rect);
      // A lifting pen reports pressure 0 on pointerup; that sample keeps the
      // last contact pressure (a zero-radius stamp makes the page renderer
      // refuse the whole Fountain stroke).
      const previous=active.stroke.points[active.stroke.points.length-1];
      // The lift repeats the last position: it adds no geometry, only a
      // changed stroke end after pen-up, so it is not stored.
      if(event.type==='pointerup'&&previous&&previous.x===x&&previous.y===y)continue;
      const pressure=item.pointerType==='mouse'?0.5:item.pressure>0?Math.min(1,item.pressure):previous?.pressure??0.5;
      active.stroke.points.push({x,y,pressure,timeMs});
    }
    const g=liveGesture.current;
    if(!g){setCurrent({...active.stroke,points:[...active.stroke.points]});return;}
    // Predicted samples only move the drawn tip; they are never stored.
    const last=active.stroke.points[active.stroke.points.length-1];
    g.predicted=(event.nativeEvent.getPredictedEvents?.()??[]).slice(0,4).map(item=>{const [x,y]=pagePoint(item,rect);return {x,y,pressure:last?.pressure??0.5,timeMs:Math.min(65535,Math.max(last?.timeMs??0,Math.round(item.timeStamp-active.started)))};});
    // A prediction is a guess about the next few milliseconds: when the pen
    // rests it must not stay on screen, so it expires on its own.
    clearTimeout(g.expire);if(g.predicted.length)g.expire=window.setTimeout(()=>{if(liveGesture.current!==g||!gesture.current)return;g.predicted=[];paintTail(g,gesture.current.stroke.points,false);},40);
    g.events.push(event.timeStamp);g.exactEvents.push(event.timeStamp);
    if(!g.frame)g.frame=requestAnimationFrame(paintLive);
  };
  // Stroke eraser: anything the eraser path touches is removed whole.
  const sweep=(event:ReactPointerEvent<SVGSVGElement>)=>{
    const active=eraseGesture.current,element=canvas.current;if(!active||!page||!element||active.pointer!==event.pointerId)return;
    const rect=element.getBoundingClientRect();if(!rect.width||!rect.height)return;
    const events=event.nativeEvent.getCoalescedEvents?.()??[event.nativeEvent];
    const start=Math.max(0,active.path.length-1);
    for(const item of events.length?events:[event.nativeEvent])active.path.push(pagePoint(item,rect));
    const segment=active.path.slice(start),radius=Math.max(1,ERASER_CSS_RADIUS*page.width/rect.width);
    const xs=segment.map(point=>point[0]),ys=segment.map(point=>point[1]);
    const sweepBox=[Math.min(...xs),Math.min(...ys),Math.max(...xs),Math.max(...ys)];
    const own=latest.current.strokes.filter(stroke=>stroke.pageId===page.id);
    for(const stroke of own)if(!active.drafts.has(stroke.id)&&strokeHit(segment,stroke.points.map(point=>[point.x,point.y]),stroke.size,radius))active.drafts.add(stroke.id);
    if(geometry?.status==='ready'&&projection?.capabilities.eraseInk){
      for(const stroke of geometry.strokes){
        if(active.saved.has(stroke.id)||erased.has(stroke.id))continue;
        const item=meta.get(stroke.id);const box=item?.bounds;const [ox,oy]=savedOffset(stroke.id);
        const pad=radius+stroke.size;
        if(box&&(sweepBox[2]<box[0]+ox-pad||sweepBox[0]>box[2]+ox+pad||sweepBox[3]<box[1]+oy-pad||sweepBox[1]>box[3]+oy+pad))continue;
        if(!strokeHit(segment,ox||oy?stroke.points.map(([x,y])=>[x+ox,y+oy] as [number,number]):stroke.points,stroke.size,radius))continue;
        if(item?.erasable!==true){active.blocked=true;continue;}
        active.saved.add(stroke.id);
      }
    }
    setHits({saved:[...active.saved],drafts:[...active.drafts]});
  };
  const endSweep=(keep:boolean)=>{
    const active=eraseGesture.current;eraseGesture.current=null;setCapturing(false);setHits(null);
    if(!active||!keep||!page)return;
    let saved=[...active.saved];
    // Samsung keeps at least one stroke on a Cloud page (empty-page semantics
    // are not established); refuse marks that would remove the last one.
    const remaining=page.strokes.filter(stroke=>!erased.has(stroke.id)).length;
    if(saved.length&&saved.length>=remaining){saved=[];setNotice('Sayfadaki son kayıtlı çizgi silinemez.');}
    else if(active.blocked)setNotice('Bazı çizgiler silinemedi.');
    else setNotice('');
    latest.current.erase(saved,[...active.drafts]);
  };
  // Lasso: drawing a loop selects strokes; dragging inside the selection
  // box moves them (clamped to the page); a tap clears the selection.
  const selectMove=(event:ReactPointerEvent<SVGSVGElement>)=>{
    const active=selectGesture.current,element=canvas.current;if(!active||!page||!element||active.pointer!==event.pointerId)return;
    const rect=element.getBoundingClientRect();if(!rect.width||!rect.height)return;
    const point=pagePoint(event.nativeEvent,rect);
    if(active.mode==='lasso'){active.path.push(point);setLasso([...active.path]);return;}
    const box=selectionBounds();let dx=Math.round(point[0]-active.start[0]),dy=Math.round(point[1]-active.start[1]);
    if(box){dx=Math.max(Math.ceil(-box[0]),Math.min(Math.floor(page.width-box[2]),dx));dy=Math.max(Math.ceil(-box[1]),Math.min(Math.floor(page.height-box[3]),dy));}
    active.offset=[dx,dy];setDrag([dx,dy]);
  };
  const endSelect=(keep:boolean)=>{
    const active=selectGesture.current;selectGesture.current=null;setCapturing(false);setLasso(null);setDrag(null);
    if(!active||!keep||!page)return;
    if(active.mode==='drag'){
      const [dx,dy]=active.offset;if(!dx&&!dy||!liveSelection)return;
      const refusal=latest.current.move(page.id,liveSelection.saved,liveSelection.drafts,dx,dy);setNotice(refusal);return;
    }
    const path=active.path;const xs=path.map(point=>point[0]),ys=path.map(point=>point[1]);
    if(path.length<3||Math.max(...xs)-Math.min(...xs)+Math.max(...ys)-Math.min(...ys)<4){setSelection(null);setNotice('');return;}
    const drafts=latest.current.strokes.filter(stroke=>stroke.pageId===page.id&&lassoHit(path,stroke.points.map(point=>[point.x,point.y]))).map(stroke=>stroke.id);
    const saved:string[]=[];let protectedHit=false;
    if(geometry?.status==='ready'&&projection?.capabilities.eraseInk)for(const stroke of geometry.strokes){
      if(erased.has(stroke.id))continue;const [ox,oy]=savedOffset(stroke.id);
      if(!lassoHit(path,stroke.points.map(([x,y])=>[x+ox,y+oy] as [number,number])))continue;
      if(meta.get(stroke.id)?.erasable!==true){protectedHit=true;continue;}
      saved.push(stroke.id);
    }
    if(saved.length>MAX_ERASE){setNotice('Tek seferde en çok 64 kayıtlı çizgi seçilebilir.');saved.length=MAX_ERASE;}
    else setNotice(protectedHit?'Bazı çizgiler seçilemedi.':geometry?.status==='failed'?'Kayıtlı çizgiler okunamadı; yalnızca yeni çizgileriniz seçilebilir.':!saved.length&&!drafts.length?'Seçim alanında çizgi yok.':'');
    setSelection(saved.length||drafts.length?{pageId:page.id,saved,drafts}:null);
  };
  // Without an image (still loading, failed or none) the ink layer sits on a
  // blank sheet of the page's own size and colour; PageStack keeps the calm
  // placeholder text underneath.
  const image=(tileFrame?.base??imageUrl)?<img className="page-image" src={tileFrame?.base??imageUrl!} alt={`Notun ${pageIndex+1}. sayfası`} onError={onImageError}/>:null;
  if(phase!=='ready'||!page||!projection)return <>{null}{null}<div className="ink-page" ref={pageBox}>{image}</div></>;
  const geometryById=new Map((geometry?.strokes??[]).map(stroke=>[stroke.id,stroke]));
  const dragOffset=(id:string,draftStroke:boolean):[number,number]=>drag&&(draftStroke?selectedDrafts.has(id):selectedSaved.has(id))?drag:[0,0];
  // Saved strokes drawn somewhere else than the page image shows them.
  const movedSaved=[...new Set([...geometryById.keys()])].filter(id=>!erased.has(id)).map(id=>{const [ox,oy]=savedOffset(id),[gx,gy]=dragOffset(id,false);return {stroke:geometryById.get(id)!,dx:ox+gx,dy:oy+gy};}).filter(item=>item.dx||item.dy);
  const masks=[...[...erased].map(id=>geometryById.get(id)),...movedSaved.map(item=>item.stroke)].filter((stroke):stroke is InkGeometryStroke=>!!stroke);
  const highlighted=(hits?.saved??[]).map(id=>geometryById.get(id)).filter((stroke):stroke is InkGeometryStroke=>!!stroke);
  const hitDrafts=new Set(hits?.drafts??[]);
  const box=selecting&&drawing?selectionBounds():null;const shownBox=box&&drag?[box[0]+drag[0],box[1]+drag[1],box[2]+drag[0],box[3]+drag[1]]:box;
  const mode=drawing&&available?(selecting?'selecting':erasing?'erasing':'drawing'):'';
  // Pen strokes the live renderer draws (same pixels as while drawing); the
  // rest (highlighters, one-sample taps, pens without a known template) stay SVG.
  const pageStrokes=[...pending,...strokes,...retainedPending.filter(item=>item.token===tileFrame?.token).map(item=>item.stroke)].filter(stroke=>stroke.pageId===page.id);
  // A stroke still being rasterized shows its pen-up pixels (handoff) or,
  // when it has none, keeps the SVG preview until its raster is ready.
  const rastered=pageStrokes.flatMap(stroke=>{const value=rasterOf(stroke);const raster=value??(value===undefined?handoffs[stroke.id]??shown.current.get(stroke.id):undefined);if(raster)shown.current.set(stroke.id,raster);else shown.current.delete(stroke.id);return raster?[{stroke,raster}]:[];});
  for(const id of [...shown.current.keys()])if(!pageStrokes.some(stroke=>stroke.id===id))shown.current.delete(id);
  const rasters=new Set(rastered.map(item=>item.stroke.id));
  const tiledDrafts:TileDraft[]=tileFrame?rastered.map(({stroke,raster})=>{const [dx,dy]=dragOffset(stroke.id,true);return {raster,dx,dy,faded:hitDrafts.has(stroke.id)};}):[];
  return <>
    {null}{null}
    <div className="ink-page" ref={pageBox} data-ink-overlay-count={pageStrokes.length} data-ink-bucket={tileFrame?.bucket}>{image}
      {tileFrame&&tileFrame.tiles.map(tile=><InkTileCanvas key={`${tile.column}:${tile.row}`} tile={tile} frame={tileFrame} pixel={pixel} drafts={tiledDrafts}/>)}
      {(masks.length>0||movedSaved.length>0)&&<svg className="ink-underlay" style={{['--ink-erase-mask' as string]:maskColor}} viewBox={`0 0 ${page.width} ${page.height}`} preserveAspectRatio="none" aria-hidden="true">
        {masks.map(stroke=><Mask key={'m'+stroke.id} className="ink-erase-mask" points={stroke.points} size={stroke.size}/>)}
        {movedSaved.map(({stroke,dx,dy})=><MovedStroke key={'v'+stroke.id} points={stroke.points} size={stroke.size} colorARGB={meta.get(stroke.id)?.colorARGB??0xff252525} dx={dx} dy={dy}/>)}
      </svg>}
      {!tileFrame&&pixel&&rastered.map(({stroke,raster})=>{const [dx,dy]=dragOffset(stroke.id,true);return <RasterCanvas key={stroke.id} id={stroke.id} raster={raster} pageWidth={page.width} pageHeight={page.height} dx={dx} dy={dy} faded={hitDrafts.has(stroke.id)} pixel={pixel} snap={false}/>;})}
      {live&&<canvas ref={liveCanvas} className="ink-live" aria-hidden="true"/>}
      <svg ref={canvas} className={`ink-overlay ${mode}${drag?" moving":""}`} style={{['--ink-erase-mask' as string]:maskColor}} viewBox={`0 0 ${page.width} ${page.height}`} preserveAspectRatio="none" aria-label={isCurrent?(selecting?'Seçim alanı':erasing?'Silgi alanı':'Çizim alanı'):`${pageIndex+1}. sayfa`} onPointerDown={event=>{
      if(!drawing||!available||gesture.current||eraseGesture.current||selectGesture.current||event.button!==0)return;
      if(selecting){
        const element=canvas.current;if(!element)return;const rect=element.getBoundingClientRect();if(!rect.width||!rect.height)return;
        const point=pagePoint(event.nativeEvent,rect);const bounds=selectionBounds();const pad=Math.max(4,10*page.width/rect.width);
        event.preventDefault();element.setPointerCapture(event.pointerId);setCapturing(true);setNotice('');
        const inside=!!bounds&&point[0]>=bounds[0]-pad&&point[0]<=bounds[2]+pad&&point[1]>=bounds[1]-pad&&point[1]<=bounds[3]+pad;
        selectGesture.current={pointer:event.pointerId,mode:inside?'drag':'lasso',start:point,path:[point],offset:[0,0]};if(!inside)setLasso([point]);return;
      }
      if(erasing){
        event.preventDefault();canvas.current?.setPointerCapture(event.pointerId);setCapturing(true);setNotice('');
        eraseGesture.current={pointer:event.pointerId,path:[],saved:new Set(),drafts:new Set(),blocked:false};sweep(event);return;
      }
      const marker=tool==='highlighter';
      const first=!marker&&!source&&pageDrawMode(projection,page)==='first';
      if(!marker&&!source&&!first)return;
      if(strokes.length>=MAX_STROKES){setNotice('Önce mevcut çizimleri kaydedin.');return;}
      event.preventDefault();canvas.current?.setPointerCapture(event.pointerId);setCapturing(true);setNotice('');
      gesture.current={pointer:event.pointerId,started:event.timeStamp,stroke:marker?{id:crypto.randomUUID(),pageId:page.id,pageSize:[page.width,page.height],sourceStrokeId:'',kind:'highlighter',colorARGB:controller.highlighterStyle.colorARGB,size:controller.highlighterStyle.size,points:[]}:{id:crypto.randomUUID(),pageId:page.id,pageSize:[page.width,page.height],sourceStrokeId:source?.id??'',kind:source?.kind??'fountain',colorARGB:controller.penStyle.colorARGB??source?.colorARGB??FIRST_STROKE_COLOR,size:controller.penStyle.size??source?.size??FIRST_STROKE_SIZE,points:[]}};
      const spec=!marker&&live&&scale&&pixel&&liveCanvas.current?specOf(gesture.current.stroke):null;
      if(spec&&pixel){
        const canvas=liveCanvas.current!;const width=tileFrame?Math.ceil(page.width*scale):Math.round(pixel.w*pixel.dpr),height=tileFrame?Math.ceil(page.height*scale):Math.round(pixel.h*pixel.dpr);if(canvas.width!==width||canvas.height!==height){canvas.width=width;canvas.height=height;}
        const {color,opacity}=inkColor(spec.colorARGB);
        liveGesture.current={spec,scale,width:spec.size,color,opacity,predicted:[],expire:0,frame:0,busy:false,rendered:0,exact:null,tail:null,events:[],exactEvents:[]};
      }
      collect(event);
    }} onPointerMove={event=>{if(gesture.current){event.preventDefault();collect(event);}else if(eraseGesture.current){event.preventDefault();sweep(event);}else if(selectGesture.current){event.preventDefault();selectMove(event);}}} onPointerUp={event=>{if(gesture.current?.pointer===event.pointerId){collect(event);finish(true);}else if(eraseGesture.current?.pointer===event.pointerId){sweep(event);endSweep(true);}else if(selectGesture.current?.pointer===event.pointerId){selectMove(event);endSelect(true);}}} onPointerCancel={event=>{if(gesture.current?.pointer===event.pointerId)finish(false);else if(eraseGesture.current?.pointer===event.pointerId)endSweep(false);else if(selectGesture.current?.pointer===event.pointerId)endSelect(false);}} onLostPointerCapture={event=>{if(gesture.current?.pointer===event.pointerId)finish(false);else if(eraseGesture.current?.pointer===event.pointerId)endSweep(false);else if(selectGesture.current?.pointer===event.pointerId)endSelect(false);}}>
      {pageStrokes.filter(stroke=>!rasters.has(stroke.id)).sort((a,b)=>Number(isHighlighter(a))-Number(isHighlighter(b))).map(stroke=>{const [dx,dy]=dragOffset(stroke.id,true);return <g key={stroke.id} className={hitDrafts.has(stroke.id)?'ink-erase-hit':undefined} transform={dx||dy?`translate(${dx} ${dy})`:undefined}><Stroke stroke={stroke}/></g>;})}{current&&<Stroke stroke={current}/>}
      {highlighted.map(stroke=><Mask key={'h'+stroke.id} className="ink-erase-target" points={stroke.points} size={stroke.size}/>)}
      {lasso&&lasso.length>1&&<polyline className="ink-lasso" points={lasso.map(point=>point.join(',')).join(' ')}/>}
      {shownBox&&<rect className="ink-selection" data-testid="ink-selection" x={shownBox[0]} y={shownBox[1]} width={Math.max(1,shownBox[2]-shownBox[0])} height={Math.max(1,shownBox[3]-shownBox[1])}/>}
    </svg></div>
  </>;
}
