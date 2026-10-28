import {test,expect, type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
import {readFile} from 'node:fs/promises';
import {homedir} from 'node:os';
import path from 'node:path';
const TOKEN='synthetic_local_session_1234567890';
const base={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:true,titleEditable:true,readOnlyReason:null,favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null};
async function setup(page:Page,beforeLoad?:(notes:any[])=>Promise<void>) {
  await page.route('**/local-fonts/*',async route=>{
    const name=new URL(route.request().url()).pathname.split('/').pop()!;
    if(!['segoeui.ttf','segoeuib.ttf','segoeuil.ttf','seguisb.ttf','segoeuisl.ttf'].includes(name))return route.abort();
    try{return route.fulfill({contentType:'font/ttf',body:await readFile(path.join(homedir(),'.local/state/samsung-notes-research/ui-reference/fonts',name))});}catch{return route.fulfill({status:404,body:''});}
  });
  let preferences={version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100};
  const state={connected:true,syncState:'idle',autoConfirm:true,capabilities:{textEditing:true,cloudDelete:false,cloudTrash:true,cloudFavorites:true,cloudFolders:true,attachments:false}};
  const folders:any[]=[{state:'normal',syncState:'synced',error:null,id:'work',name:'Çalışma',parentId:null,color:null,noteCount:0},{state:'normal',syncState:'synced',error:null,id:'ideas',name:'Fikirler',parentId:'work',color:null,noteCount:0}];
  let notes:any[]=[{...base,id:'one',title:'Aklımda kalanlar',body:'Yavaş bir sabah, açık bir pencere.\n\nBugün üzerinde düşünmek istediğim küçük fikirler.',preview:'Yavaş bir sabah, açık bir pencere.'},{...base,id:'two',title:'Hafta sonu için',body:'Kitapçıya uğra.\nYeni bir yürüyüş rotası bul.',preview:'Kitapçıya uğra. Yeni bir yürüyüş rotası bul.',favorite:true},{...base,id:'rich',title:'Tablet çizimleri',body:'Çizim notunun metin önizlemesi.',preview:'Çizim notunun metin önizlemesi.',editable:false,titleEditable:false,readOnlyReason:'Bu not çizimler içeriyor. Özgün biçim korunur.'},{...base,id:'conflict',title:'İki farklı sürüm',body:'Yerel metin',preview:'Yerel metin',syncState:'conflict',remoteBody:'Uzak metin',remoteTitle:'Uzak başlık'}];
  const requests:{method:string;path:string;data:any}[]=[];
  await page.route('**/api/**',async route=>{
    expect(route.request().headers()['authorization']).toBe(`Bearer ${TOKEN}`);
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method();
    const data=method==='GET'?null:route.request().postDataJSON();requests.push({method,path,data});
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/preferences'){if(method==='PUT')preferences=data;return send(preferences);}
    if(path==='/api/folders'&&method==='GET')return send({folders:folders.filter(f=>f.state==='normal').map(f=>({...f,noteCount:notes.filter(n=>n.folderId===f.id&&!n.trashed).length})),trash:folders.filter(f=>f.state==='trashed'),initialized:true});
    if(path==='/api/folders'&&method==='POST'){const folder={id:'created-folder',...data,color:null,noteCount:0,state:'normal',syncState:'pending',error:null};folders.push(folder);return send({folder},201);}
    if(path.startsWith('/api/folders/')&&path.endsWith('/resolve')){const folder=folders.find(f=>f.id===path.split('/')[3]);folder.syncState='synced';folder.error=null;return send({folder});}
    if(path.startsWith('/api/folders/')&&method==='PATCH'){const folder=folders.find(f=>f.id===path.split('/')[3]);if(!folder)return send({error:{message:'Klasör bulunamadı'}},404);Object.assign(folder,data,{syncState:'pending'});if('trashed' in data)folder.state=data.trashed?'trashed':'normal';return send({folder});}
    if(path==='/api/status'){
      if(state.autoConfirm)for(const folder of folders)if(folder.syncState==='pending')folder.syncState='synced';
      if(state.autoConfirm)for(const note of notes)if(note.metadataSync.state==='pending'){note.metadataSync={state:'synced',error:null};note.syncState='synced';}
      return send({connected:state.connected,accountLabel:'Test hesabı',loginPending:false,sync:{state:state.syncState,lastSyncedAt:1777000000000,pending:notes.filter(n=>n.syncState==='pending').length,conflicts:1,progress:{processed:0,total:0},error:null},capabilities:state.capabilities});
    }
    if(path==='/api/notes'&&method==='GET'){const f=url.searchParams.get('filter'),q=url.searchParams.get('q')||'';return send({notes:notes.filter(n=>(f==='trash'?n.trashed:!n.trashed)&&(!url.searchParams.get('folder')||n.folderId===url.searchParams.get('folder'))&&(f!=='favorites'||n.favorite)&&(f!=='conflicts'||n.syncState==='conflict')&&(n.title+n.body).toLowerCase().includes(q.toLowerCase())).map(n=>({...n,body:undefined}))});}
    if(path==='/api/notes'&&method==='POST'){const note={...base,id:`new-${notes.length}`,source:'local',syncState:'local',title:data.title,body:data.body,preview:data.body.slice(0,80)};notes.unshift(note);return send({note},201);}
    const id=path.split('/')[3],note=notes.find(n=>n.id===id);if(!note)return send({error:{code:'missing',message:'Not bulunamadı.'}},404);
    if(path.endsWith('/export'))return route.fulfill({contentType:'text/plain',body:note.body});
    if(path.endsWith('/export-file')&&method==='POST')return send({path:`/srv/example-home/Downloads/${note.title}.${data.format}`,name:`${note.title}.${data.format}`,pages:0,skipped:0});
    if(method==='PUT'){if(data.expectedRevision!==note.revision)return send({error:{code:'revision_conflict',message:'Sürüm değişti.'}},409);await new Promise(r=>setTimeout(r,180));Object.assign(note,{...data,revision:note.revision+1,syncState:'pending',preview:data.body.slice(0,80)});return send({note});}
    if(path.endsWith('/title')&&method==='PATCH'){if(!note.titleEditable||data.expectedRevision!==note.revision)return send({error:{code:'revision_conflict',message:'Sürüm değişti.'}},409);Object.assign(note,{title:data.title,revision:note.revision+1,syncState:'pending'});return send({note});}
    if(method==='PATCH'){if(data.expectedRevision!==note.revision)return send({error:{code:'revision_conflict',message:'Sürüm değişti.'}},409);Object.assign(note,{...data,revision:note.revision+1,syncState:'pending',metadataSync:{state:'pending',error:null}});return send({note});}
    if(path.endsWith('/resolve')){note.syncState='synced';note.metadataSync={state:'synced',error:null};if(data.strategy==='use_remote')note.body=note.remoteBody;return send({note});}
    return send({note});
  });
  await beforeLoad?.(notes);
  await page.goto('/#token='+TOKEN);await page.evaluate(()=>document.fonts.ready);
  await expect(page.getByRole('button',{name:/Aklımda kalanlar/})).toBeVisible();
  return {requests,notes,state,folders};
}
async function openNote(page:Page,name:RegExp){
  const back=page.getByRole('button',{name:'Not listesine dön'});
  if(await back.isVisible())await back.click();
  await page.getByRole('button',{name}).click();
}
const folderOption=(page:Page,list:string,name:string)=>page.getByRole('listbox',{name:list}).getByRole('option',{name:new RegExp('^'+name)});
async function pickFolder(page:Page,list:string,name:string){await folderOption(page,list,name).click();await expect(folderOption(page,list,name)).toHaveAttribute('aria-selected','true');}
async function noteAction(page:Page,name:string){
  const action=page.getByRole('button',{name,exact:true});
  if(!await action.isVisible())await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await action.click();
}
test('Windows surfaces use restrained corners and reduced-motion respects navigation',async({page})=>{
  const errors:string[]=[];page.on('pageerror',error=>errors.push(error.name));
  await setup(page);
  await expect(page).toHaveTitle(/Not/);
  await expect(page.locator('.note-thumbnail').first()).toHaveCSS('border-radius','6px');
  await page.getByRole('button',{name:'Ayarlar',exact:true}).click();
  const dialog=page.getByRole('dialog',{name:'Ayarlar'});
  await expect(dialog).toBeVisible();await expect(dialog).toHaveCSS('border-radius','6px');
  // Surfaces open without an enter animation (on WebKitGTK/Wayland its compositing layer flickered).
  await expect(dialog).toHaveCSS('animation-name','none');
  await dialog.evaluate(element=>Promise.all(element.getAnimations().map(animation=>animation.finished)));
  await page.screenshot({path:'/tmp/notes-settings-corners.png'});
  await page.getByRole('button',{name:'Ayarları kapat'}).click();
  await openNote(page,/Aklımda kalanlar/);
  // Switching between the list and a note is instant: no panel slide (it read as surfaces "going and coming back").
  await expect(page.locator('.editor-panel')).toHaveCSS('animation-name','none');
  await page.getByRole('button',{name:'Not listesine dön'}).click();
  await expect(page.locator('.note-list')).toHaveCSS('animation-name','none');
  await page.emulateMedia({reducedMotion:'reduce'});
  await page.getByRole('button',{name:'Ayarlar',exact:true}).click();
  await expect(dialog).toHaveCSS('animation-name','none');
  await page.getByRole('button',{name:'Ayarları kapat'}).click();
  await openNote(page,/Aklımda kalanlar/);
  await expect(page.locator('.editor-panel')).toHaveCSS('animation-name','none');
  await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await expect(page.locator('.note-menu')).toHaveCSS('animation-name','none');
  await expect(page.getByRole('button',{name:'Favorilere ekle',exact:true})).toBeEnabled();
  expect(errors).toEqual([]);
});
test('cloud trash stays pending until confirmation, then restores through a separate revisioned request',async({page})=>{
  const {notes,requests,state}=await setup(page);state.autoConfirm=false;
  await openNote(page,/Aklımda kalanlar/);await noteAction(page,'Çöp kutusuna taşı');
  await expect(page.getByText('Çöp kutusu işleminin bulut sonucu bekleniyor.')).toBeVisible();
  await expect(page.getByRole('button',{name:'Geri yükle',exact:true})).toBeDisabled();
  await expect(page.getByText('Bu not Samsung Cloud çöp kutusunda.')).toHaveCount(0);
  expect(requests.find(r=>r.method==='PATCH')?.data).toEqual({trashed:true,expectedRevision:1});
  const note=notes.find(n=>n.id==='one');note.metadataSync={state:'synced',error:null};note.syncState='synced';
  await expect(page.getByText('Bu not Samsung Cloud çöp kutusunda.')).toBeVisible({timeout:7000});
  await page.getByRole('button',{name:'Geri yükle',exact:true}).click();
  await expect(page.getByText('Düzenleme işlemi buluta gönderilmeyi bekliyor.',{exact:false})).toBeVisible();
  expect(requests.filter(r=>r.method==='PATCH').map(r=>r.data)).toEqual([{trashed:true,expectedRevision:1},{trashed:false,expectedRevision:2}]);
});
test('offline cloud favorites queue safely and folder move uses actual folder identifiers',async({page})=>{
  const {notes,requests,state}=await setup(page);state.autoConfirm=false;state.connected=false;state.syncState='offline';
  await page.reload();await openNote(page,/Aklımda kalanlar/);await noteAction(page,'Favorilere ekle');
  await expect(page.getByText(/Bağlantı kurulduğunda devam edecek/)).toBeVisible();
  await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await expect(page.getByRole('button',{name:'Favorilerden çıkar',exact:true})).toBeDisabled();
  await expect(page.getByRole('button',{name:'Klasöre taşı',exact:true})).toBeDisabled();
  await page.getByRole('button',{name:'Not seçenekleri'}).click();
  const note=notes.find(n=>n.id==='one');note.metadataSync={state:'synced',error:null};note.syncState='synced';
  await expect(page.getByText('Düzenleme işlemi buluta gönderilmeyi bekliyor.',{exact:false})).toHaveCount(0,{timeout:7000});
  await noteAction(page,'Klasöre taşı');await pickFolder(page,'Hedef klasör','Fikirler');
  await page.getByRole('button',{name:'Taşı',exact:true}).click();
  expect(requests.filter(r=>r.method==='PATCH').at(-1)?.data).toEqual({folderId:'ideas',expectedRevision:2});
  await page.getByRole('button',{name:'Not listesine dön'}).click();
  await page.getByRole('button',{name:'Çalışma klasörünü genişlet'}).click();await page.locator('.folder-item').filter({hasText:'Fikirler'}).click();
  await expect(page.getByRole('heading',{name:'Fikirler',exact:true})).toBeVisible();
  await expect(page.getByRole('button',{name:/Aklımda kalanlar/})).toBeVisible();
  await expect(page.getByRole('button',{name:/Hafta sonu için/})).toHaveCount(0);
});
test('cloud capabilities gate metadata and settings never describe it as local-only',async({page})=>{
  const {state,requests}=await setup(page);state.capabilities.cloudTrash=false;state.capabilities.cloudFavorites=false;state.capabilities.cloudFolders=false;
  await page.reload();await openNote(page,/Aklımda kalanlar/);await page.getByRole('button',{name:'Not seçenekleri'}).click();
  for(const name of ['Favorilere ekle','Çöp kutusuna taşı','Klasöre taşı'])await expect(page.getByRole('button',{name,exact:true})).toBeDisabled();
  expect(requests.filter(r=>r.method==='PATCH')).toHaveLength(0);
  await page.getByRole('button',{name:'Not listesine dön'}).click();await page.getByRole('button',{name:'Ayarlar',exact:true}).click();
  await expect(page.getByText(/yüklenmiş notları çevrimdışıyken/)).toBeVisible();
  await expect(page.getByText(/yalnızca bu kütüphanede saklanır/)).toHaveCount(0);
});
test('polling refreshes clean content but preserves dirty drafts when remote revision changes',async({page})=>{
  const {notes}=await setup(page);await openNote(page,/Aklımda kalanlar/);
  const note=notes.find(n=>n.id==='one');Object.assign(note,{title:'Buluttaki yeni başlık',body:'Buluttaki yeni içerik',preview:'Buluttaki yeni içerik',revision:2});
  await expect(page.getByLabel('Not içeriği')).toHaveValue('Buluttaki yeni içerik',{timeout:7000});
  await expect(page.getByLabel('Not başlığı')).toHaveValue('Buluttaki yeni başlık');
  await page.getByLabel('Not içeriği').fill('Korunacak taslak');Object.assign(note,{body:'Daha yeni bulut içeriği',revision:3});
  await page.waitForResponse(async response=>response.url().endsWith('/api/notes/one')&&response.request().method()==='GET'&&(await response.json()).note.revision===3);await expect(page.getByLabel('Not içeriği')).toHaveValue('Korunacak taslak');
  await page.getByRole('button',{name:'Not seçenekleri'}).click();await expect(page.getByRole('button',{name:'Favorilere ekle',exact:true})).toBeDisabled();await expect(page.getByRole('button',{name:'Klasöre taşı',exact:true})).toBeDisabled();await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await page.keyboard.press('ControlOrMeta+s');await expect(page.getByText(/Notun başka bir sürümü kaydedildi/)).toBeVisible();
  await expect(page.getByLabel('Not içeriği')).toHaveValue('Korunacak taslak');
});
test('partial bulk failure keeps only failed notes selected and never claims cloud completion',async({page})=>{
  const {requests,state}=await setup(page);state.autoConfirm=false;
  await page.route('**/api/notes/two',route=>route.request().method()==='PATCH'?route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{code:'unavailable',message:'Sentetik hata'}})}):route.fallback());
  await page.getByRole('button',{name:'Düzenle',exact:true}).click();
  await page.getByRole('button',{name:/Aklımda kalanlar/}).click();await page.getByRole('button',{name:/Hafta sonu için/}).click();
  await page.getByRole('button',{name:'Favorilere ekle',exact:true}).click();
  await expect(page.getByText('1 not seçildi',{exact:true})).toBeVisible();
  await expect(page.getByText(/1 not için işlem kuyruğa alınamadı/)).toBeVisible();
  // Samsung clients save silently; only the failure is reported, never a success claim.
  await expect(page.getByText(/kuyruğa alındı/)).toHaveCount(0);
  expect(requests.filter(r=>r.method==='PATCH')).toHaveLength(1);
});
test('secure session, real list/detail and draft survives switching and polling',async({page})=>{
  const {requests}=await setup(page);
  expect(new URL(page.url()).hash).toBe('');
  await openNote(page,/Aklımda kalanlar/);
  await page.getByLabel('Not içeriği').fill('Kaydedilmeyen Türkçe düşünce 🌱');
  await openNote(page,/Hafta sonu için/);
  await openNote(page,/Aklımda kalanlar/);
  await expect(page.getByLabel('Not içeriği')).toHaveValue('Kaydedilmeyen Türkçe düşünce 🌱');
  await page.waitForTimeout(4200);
  await expect(page.getByLabel('Not içeriği')).toHaveValue('Kaydedilmeyen Türkçe düşünce 🌱');
  await page.keyboard.press('ControlOrMeta+s');
  await expect(page.getByText('Buluta gönderilmeyi bekliyor',{exact:true})).toBeVisible();
  expect(requests.find(r=>r.method==='PUT')?.data.expectedRevision).toBe(1);
  await page.screenshot({path:'tests/artifacts/workspace-desktop.png',fullPage:true});
});
test('typing during serialized save stays dirty and uses updated revision',async({page})=>{
  const {requests}=await setup(page);await openNote(page,/Aklımda kalanlar/);
  await page.getByLabel('Not içeriği').fill('Birinci');await page.keyboard.press('ControlOrMeta+s');
  await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await expect(page.getByRole('button',{name:'Favorilere ekle',exact:true})).toBeDisabled();
  await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await page.getByLabel('Not içeriği').fill('İkinci');await expect(page.getByRole('button',{name:'Kaydet',exact:true})).toBeEnabled();
  await page.keyboard.press('ControlOrMeta+s');await expect(page.locator('.editor-footer').getByText('Kaydedildi',{exact:true})).toBeVisible();
  expect(requests.filter(r=>r.method==='PUT').map(r=>r.data.expectedRevision)).toEqual([1,2]);
});
test('readonly copy leaves original untouched, favorite and recoverable trash work',async({page})=>{
  const {requests}=await setup(page);await openNote(page,/Tablet çizimleri/);
  await expect(page.getByLabel('Not içeriği')).toHaveAttribute('readonly','');
  // One calm sentence, no mode switch and no jargon about extraction.
  await expect(page.getByText('Bu notun sayfaları burada gösterilemiyor; yalnızca metni görünüyor.',{exact:true})).toBeVisible();
  await expect(page.getByText(/çıkarılabilen|Yalnızca metin önizlemesi/)).toHaveCount(0);await expect(page.getByRole('button',{name:'Metni göster'})).toHaveCount(0);
  await expect(page.getByRole('button',{name:'Dışa aktar',exact:true})).toHaveAttribute('title','Metin dosyası olarak kaydet');
  await page.screenshot({path:'tests/artifacts/readonly-text-preview.png',fullPage:true});
  await noteAction(page,'Metni yeni nota kopyala');
  await expect(page.getByLabel('Not başlığı')).toHaveValue('Tablet çizimleri — kopya');
  await expect(page.getByLabel('Not içeriği')).toBeEditable();
  expect(requests.some(r=>r.method==='PUT'&&r.path.endsWith('/rich'))).toBe(false);
  await page.getByRole('button',{name:'Not seçenekleri'}).click();await expect(page.getByRole('button',{name:'Favorilere ekle',exact:true})).toBeDisabled();await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await page.getByRole('button',{name:'Not listesine dön'}).click();await page.locator('.note-row').filter({has:page.getByText('Tablet çizimleri',{exact:true})}).click();await noteAction(page,'Favorilere ekle');await page.getByRole('button',{name:'Not seçenekleri'}).click();await expect(page.getByRole('button',{name:'Favorilerden çıkar'})).toBeEnabled();await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await noteAction(page,'Çöp kutusuna taşı');
  await expect(page.getByText('Bu not Samsung Cloud çöp kutusunda.')).toBeVisible();
  await page.getByRole('button',{name:'Geri yükle',exact:true}).first().click();
  await expect(page.getByText('Bu not Samsung Cloud çöp kutusunda.')).toHaveCount(0);
});
test('conflict actions and mobile workspace have real functional states',async({page})=>{
  await setup(page);await openNote(page,/İki farklı sürüm/);
  await expect(page.getByRole('button',{name:'Kaydet',exact:true})).toBeDisabled();
  await page.getByText('Buluttaki metni karşılaştır').click();await expect(page.getByText('Uzak başlık',{exact:false})).toBeVisible();
  await page.getByRole('button',{name:'Buluttaki sürümü kullan',exact:true}).click();await expect(page.getByLabel('Not içeriği')).toHaveValue('Uzak metin');
  await page.setViewportSize({width:390,height:844});
  await expect(page.getByRole('button',{name:'Not listesine dön'})).toBeVisible();
  await expect(page.getByLabel('Not içeriği')).toBeVisible();
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  await page.screenshot({path:'tests/artifacts/workspace-mobile.png',fullPage:true});
  await page.getByRole('button',{name:'Not listesine dön'}).click();await expect(page.getByLabel('Notlarında ara')).toBeVisible();
});
test('missing token fails closed without API calls',async({page})=>{
  const apiCalls:string[]=[];page.on('request',r=>{if(r.url().includes('/api/'))apiCalls.push(r.url());});
  await page.goto('/');await expect(page.getByText('Notlarınıza güvenli bir giriş')).toBeVisible();expect(apiCalls).toEqual([]);
});
test('search, favorites and authorized text export use contract endpoints',async({page})=>{
  const {requests}=await setup(page);
  await page.getByLabel('Notlarında ara').fill('Hafta');
  await expect(page.getByRole('button',{name:/Aklımda kalanlar/})).toHaveCount(0);
  await openNote(page,/Hafta sonu için/);
  await page.getByRole('button',{name:'Dışa aktar',exact:true}).click();
  // The desktop engine writes the file itself; the UI names where it went.
  await expect(page.getByText('İndirilenler klasörüne kaydedildi: Hafta sonu için.txt')).toBeVisible();
  expect(requests.some(r=>r.path==='/api/notes/two/export-file')).toBe(true);
  await page.getByRole('button',{name:'Not listesine dön'}).click();await page.getByRole('button',{name:'Aramayı temizle'}).click();
  await page.getByRole('button',{name:'Görünüm',exact:true}).click();await page.getByRole('button',{name:'Favoriler',exact:true}).click();
  await expect(page.getByRole('button',{name:/Hafta sonu için/})).toBeVisible();
  await expect(page.getByRole('button',{name:/Aklımda kalanlar/})).toHaveCount(0);
});
test('disconnected library remains usable and login opens only through API',async({page})=>{
  await setup(page);let pending=false;
  await page.route('**/api/status',route=>route.fulfill({contentType:'application/json',body:JSON.stringify({connected:false,accountLabel:null,loginPending:pending,sync:{state:'offline',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudDelete:false,attachments:false}})}));
  await page.route('**/api/auth/login',route=>{pending=true;return route.fulfill({status:202,contentType:'application/json',body:'{"started":true}'});});
  await page.reload();await expect(page.getByText('Çevrimdışı',{exact:true})).toBeVisible();
  await page.getByRole('button',{name:'Yeni not',exact:false}).first().click();
  await expect(page.getByLabel('Not içeriği')).toBeEditable();
  await page.getByRole('button',{name:'Not listesine dön'}).click();await page.getByRole('button',{name:'Ayarlar',exact:true}).click();await page.getByRole('button',{name:'Samsung hesabıyla bağlan',exact:true}).click();
  await expect(page.getByRole('button',{name:'Tarayıcıda giriş bekleniyor',exact:false})).toBeDisabled();
});

test('drawing-only notes never invite typing or pretend to render an empty page',async({page})=>{
  await setup(page);
  await page.route('**/api/notes/rich',route=>route.fulfill({contentType:'application/json',body:JSON.stringify({note:{...base,id:'rich',title:'Yalnız çizim içeren not',body:'',preview:'',editable:false,readOnlyReason:'Bu not çizimler içeriyor.'}})}));
  await openNote(page,/Tablet çizimleri/);
  await expect(page.getByLabel('Not içeriği')).toHaveValue('');
  await expect(page.getByLabel('Not içeriği')).toHaveAttribute('placeholder','Bu notta metin yok.');
  await expect(page.getByText('Bu notun sayfaları burada gösterilemiyor.',{exact:true})).toBeVisible();
  await page.getByRole('button',{name:'Not seçenekleri'}).click();await expect(page.getByRole('button',{name:'Metni yeni nota kopyala'})).toBeDisabled();await expect(page.getByRole('button',{name:'Metni yeni nota kopyala'})).toHaveAttribute('title','Bu notta kopyalanacak metin yok.');await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await expect(page.locator('.editor-footer').getByText('Salt okunur',{exact:true})).toBeVisible();
  await expect(page.getByRole('button',{name:'Sayfalar',exact:true})).toBeDisabled();await expect(page.getByRole('button',{name:'Sayfalar',exact:true})).toHaveAttribute('title','Bu notta sayfa yok');
});
test('Windows reference layout captures use synthetic content and local font faces',async({page})=>{
  await page.setViewportSize({width:1920,height:1014});
  const {notes}=await setup(page);
  const titles=['Sabah notları','Okuma listesi','Haftalık plan','Yeni fikirler','Çalışma notları','Yolculuk planı','Toplantı özeti','Alışveriş listesi','Proje taslağı','Küçük hatırlatmalar'];
  notes.splice(0,notes.length,...Array.from({length:37},(_,i)=>({...base,id:`visual-${i}`,title:i===0?'LinuxSync-Probe-001':titles[i%titles.length]+' '+(i+1),body:i===0?'Android test 001':'Bu, arayüz kontrolü için oluşturulmuş sentetik bir nottur.\nGerçek kullanıcı içeriği değildir.',preview:i===0?'Android test 001':'Bu, arayüz kontrolü için oluşturulmuş sentetik bir nottur.\nGerçek kullanıcı içeriği değildir.',modifiedAt:1777000000000+i*86400000})));
  await page.reload();await page.evaluate(()=>document.fonts.ready);await expect(page.locator('.note-row')).toHaveCount(37);
  await page.screenshot({path:'tests/artifacts/windows-clone-list.png',fullPage:true});
  const card=await page.locator('.note-thumbnail').first().boundingBox();expect(card?.x).toBe(305);expect(card?.y).toBe(88);expect(card?.width).toBe(125);
  await openNote(page,/LinuxSync-Probe-001/);await expect(page.getByLabel('Not içeriği')).toHaveValue('Android test 001');
  await page.evaluate(()=>document.fonts.ready);await page.screenshot({path:'tests/artifacts/windows-clone-editor.png',fullPage:true});
  const paper=await page.locator('.document').boundingBox();expect(paper?.width).toBeCloseTo(718,2); // macOS reports 718.00006 (float layout rounding)
  await expect(page.getByRole('button',{name:'Kalem',exact:true})).toBeDisabled();
  await expect(page.getByLabel('Yazı boyutu')).toHaveCount(0);await expect(page.getByText('Bu notta yazı biçimlendirme şimdilik kullanılamıyor.',{exact:true})).toBeVisible();
});
test('all pages stack like Samsung Notes: pending images poll, missing pages say so, dark pages stay dark, URLs are revoked',async({page})=>{
  await setup(page);let attempts=0;
  await page.route('**/api/notes/rich/editor',route=>route.fulfill({json:{schemaVersion:1,capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:false},model:null,reason:'Bilinmeyen biçimlendirme sözdizimi.',revision:1}}));
  await page.evaluate(()=>{const original=URL.revokeObjectURL; (window as any).__revoked=[];URL.revokeObjectURL=url=>{(window as any).__revoked.push(url);original(url);};});
  await page.route('**/api/notes/rich/pages',route=>route.fulfill({contentType:'application/json',body:JSON.stringify({providerAvailable:true,pages:[{index:0,width:1,height:1,available:true},{index:1,width:null,height:null,available:false},{index:2,width:null,height:null,available:false},{index:3,width:1,height:1,available:true,darkMode:true,previewKind:'pdf-background',complete:false,previewNotice:'PDF zemini gösteriliyor; çizimler henüz eklenmedi.'}]})}));
  await page.route('**/api/notes/rich/preview?*',route=>{
    expect(route.request().headers()['authorization']).toBe(`Bearer ${TOKEN}`);attempts++;
    if(attempts===1)return route.fulfill({status:202,contentType:'application/json',body:'{"state":"pending"}'});
    return route.fulfill({contentType:'image/png',body:Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64')});
  });
  await openNote(page,/Tablet çizimleri/);
  await expect(page.locator('.page-sheet')).toHaveCount(4);
  await expect(page.locator('.page-sheet[data-page-index="0"] .page-image')).toBeVisible();expect(attempts).toBeGreaterThanOrEqual(2);
  await expect(page.locator('.page-sheet[data-page-index="1"]')).toContainText('Bu sayfanın görüntüsü bu cihazda oluşturulamıyor.');
  await expect(page.locator('.editor-footer').getByText('Salt okunur',{exact:true})).toBeVisible();await expect(page.getByRole('complementary',{name:'Kalem araçları'})).toBeVisible();await expect(page.getByText('Bilinmeyen biçimlendirme sözdizimi.',{exact:true})).toHaveCount(0);await expect(page.getByText(/düzenlenemiyor|biçimlendirme/)).toHaveCount(0);
  await page.getByRole('button',{name:'Sayfalar',exact:true}).click();await expect(page.getByLabel('Gösterilen sayfa')).toBeFocused();
  await page.emulateMedia({media:'print'});await expect(page.locator('.page-navigation')).toBeHidden();await expect(page.locator('.page-sheet[data-page-index="0"] .page-image')).toBeVisible();await page.emulateMedia({media:'screen'});
  // A page note has no 'show text' mode: pages are the note.
  await expect(page.getByRole('button',{name:'Metni göster',exact:true})).toHaveCount(0);await page.getByLabel('Gösterilen sayfa').selectOption('3');
  await expect(page.getByAltText('Notun 4. sayfası')).toBeVisible();await expect(page.getByText('Bu sayfadaki çizimler henüz gösterilemiyor; yalnızca PDF görünüyor.',{exact:true})).toBeVisible();
  await expect(page.getByLabel('Gösterilen sayfa').locator('option')).toHaveCount(4);await expect(page.getByLabel('Gösterilen sayfa').locator('option[value="1"]')).toHaveJSProperty('disabled',true);
  await expect(page.locator('.page-sheet[data-page-index="3"]')).toHaveCSS('background-color','rgb(0, 0, 0)');await expect(page.locator('.page-sheet[data-page-index="0"]')).toHaveCSS('background-color','rgb(255, 255, 255)');
  const url=await page.getByAltText('Notun 4. sayfası').getAttribute('src');expect(url).toMatch(/^blob:/);
  await page.getByRole('button',{name:'Not listesine dön'}).click();
  await expect.poll(()=>page.evaluate(()=> (window as any).__revoked)).toContain(url);
});

test('preview cancellation on note switch never replaces the next note',async({page})=>{
  await setup(page);let attempts=0;
  await page.route('**/api/notes/rich/pages',route=>route.fulfill({contentType:'application/json',body:JSON.stringify({providerAvailable:true,pages:[{index:0,width:800,height:1000,available:true}]})}));
  await page.route('**/api/notes/rich/preview?*',route=>{attempts++;return route.fulfill({status:202,contentType:'application/json',body:'{"state":"pending"}'});});
  await openNote(page,/Tablet çizimleri/);await expect(page.locator('.page-sheet')).toHaveCount(1);
  await expect.poll(()=>attempts).toBeGreaterThanOrEqual(1);await openNote(page,/Aklımda kalanlar/);
  // Polling for the previous note's page stops as soon as another note opens.
  await page.waitForTimeout(400);const stopped=attempts;await page.waitForTimeout(1500);expect(attempts).toBe(stopped);
  await expect(page.locator('.page-image')).toHaveCount(0);await expect(page.getByLabel('Not içeriği')).toBeEditable();
});
test('bulk favorites advance existing draft revision without losing unsaved text',async({page})=>{
  const {requests}=await setup(page);await openNote(page,/Aklımda kalanlar/);
  await page.getByLabel('Not içeriği').fill('Toplu seçimden önce korunacak taslak');
  await page.getByRole('button',{name:'Not listesine dön'}).click();
  await page.getByRole('button',{name:'Düzenle',exact:true}).click();
  await page.getByRole('button',{name:/Aklımda kalanlar/}).click();
  await page.getByRole('button',{name:'Favorilere ekle',exact:true}).click();
  await expect(page.getByRole('button',{name:'Düzenle',exact:true})).toBeVisible();
  await openNote(page,/Aklımda kalanlar/);await expect(page.getByLabel('Not içeriği')).toHaveValue('Toplu seçimden önce korunacak taslak');
  await page.keyboard.press('ControlOrMeta+s');await expect(page.locator('.editor-footer').getByText('Kaydedildi',{exact:true})).toBeVisible();
  expect(requests.find(r=>r.method==='PUT')?.data.expectedRevision).toBe(2);
});

test('folder lifecycle queues exact operations and disables pending changes',async({page})=>{
  test.setTimeout(60000);
  const {requests,folders,state}=await setup(page);state.autoConfirm=false;
  await page.getByRole('button',{name:'Klasör oluştur',exact:true}).click();
  await page.getByLabel('Klasör adı').fill('Yeni klasör');await pickFolder(page,'Üst klasör','Çalışma');
  await page.getByRole('button',{name:'Oluştur',exact:true}).click();
  await expect(page.locator('.folder-item').filter({hasText:'Yeni klasör'})).toBeVisible();
  expect(requests.find(r=>r.path==='/api/folders'&&r.method==='POST')?.data).toEqual({name:'Yeni klasör',parentId:'work'});
  await page.locator('.folder-item').filter({hasText:'Yeni klasör'}).click({button:'right'});await expect(page.getByRole('menuitem',{name:'Yeniden adlandır',exact:true})).toBeDisabled();
  const folder=folders.find(f=>f.id==='created-folder');folder.syncState='synced';
  await expect(page.getByRole('menuitem',{name:'Yeniden adlandır',exact:true})).toBeEnabled({timeout:7000});
  await page.getByRole('menuitem',{name:'Yeniden adlandır',exact:true}).click();await page.getByLabel('Klasör adı').fill('Yenilenen klasör');await page.getByRole('button',{name:'Kaydet',exact:true}).click();
  expect(requests.filter(r=>r.method==='PATCH').at(-1)?.data).toEqual({name:'Yenilenen klasör'});
  folder.syncState='synced';await page.locator('.folder-item').filter({hasText:'Yenilenen klasör'}).click({button:'right'});await expect(page.getByRole('menuitem',{name:'Klasörü taşı',exact:true})).toBeEnabled({timeout:7000});await page.getByRole('menuitem',{name:'Klasörü taşı',exact:true}).click();
  await expect(folderOption(page,'Üst klasör','Yenilenen klasör')).toHaveCount(0);await pickFolder(page,'Üst klasör','Fikirler');await page.getByRole('button',{name:'Kaydet',exact:true}).click();
  expect(requests.filter(r=>r.method==='PATCH').at(-1)?.data).toEqual({parentId:'ideas'});
  folder.syncState='synced';await page.locator('.folder-item').filter({hasText:'Yenilenen klasör'}).click({button:'right'});await expect(page.getByRole('menuitem',{name:'Çöp kutusuna taşı',exact:true})).toBeEnabled({timeout:7000});await page.getByRole('menuitem',{name:'Çöp kutusuna taşı',exact:true}).click();await page.getByRole('dialog').getByRole('button',{name:'Çöp kutusuna taşı',exact:true}).click();
  await page.getByRole('button',{name:'Çöp kutusu',exact:true}).click();await expect(page.getByText('Öğelerde, kalıcı olarak silinmelerine kalan gün sayısı gösterilir.')).toBeVisible();const trashCard=page.locator('.trash-folder-card').filter({hasText:'Yenilenen klasör'});await trashCard.click({button:'right'});await expect(page.getByRole('menuitem',{name:'Geri yükle',exact:true})).toBeDisabled();await page.keyboard.press('Escape');
  expect(requests.filter(r=>r.method==='PATCH').at(-1)?.data).toEqual({trashed:true});
  folder.syncState='synced';await expect(trashCard).not.toContainText('Buluta gönderilmeyi',{timeout:7000});await trashCard.click({button:'right'});await expect(page.getByRole('menuitem',{name:'Geri yükle',exact:true})).toBeEnabled();await page.getByRole('menuitem',{name:'Geri yükle',exact:true}).click();await page.getByRole('dialog').getByRole('button',{name:'Geri yükle',exact:true}).click();
  expect(requests.filter(r=>r.method==='PATCH').at(-1)?.data).toEqual({trashed:false});
});
test('metadata uncertainty offers remote refresh without text conflict resolution',async({page})=>{
  const {notes,requests,state}=await setup(page);state.autoConfirm=false;const note=notes.find(n=>n.id==='one');note.metadataSync={state:'uncertain',error:'Sentetik gönderim belirsizliği'};note.syncState='conflict';
  await openNote(page,/Aklımda kalanlar/);await expect(page.getByRole('button',{name:'Yerel sürümü kopya olarak sakla'})).toHaveCount(0);await expect(page.getByRole('button',{name:'Buluttaki sürümü kullan'})).toHaveCount(0);
  await page.getByRole('button',{name:'Buluttaki durumu kullan'}).click();await expect(page.getByText('Buluttaki düzenleme durumu doğrulanmalı.')).toHaveCount(0);
  expect(requests.find(r=>r.path.endsWith('/resolve'))?.data).toEqual({strategy:'use_remote'});
});

test('uncertain folder recovers server state without retrying its mutation',async({page})=>{
  const {folders,requests}=await setup(page);folders[0].syncState='uncertain';folders[0].error='Sentetik sonuç belirsiz';await page.reload();
  await page.locator('.folder-item').filter({hasText:'Çalışma'}).click({button:'right'});await expect(page.getByRole('menuitem',{name:'Yeniden adlandır'})).toBeDisabled();
  await page.getByRole('menuitem',{name:'Buluttaki durumu kullan'}).click();
  await expect(page.getByText('Buluttaki klasör durumu yeniden alındı.')).toBeVisible();
  expect(requests.filter(r=>r.method==='PATCH')).toHaveLength(0);expect(requests.find(r=>r.path==='/api/folders/work/resolve')?.data).toEqual({strategy:'use_remote'});
});

test('ink and PDF title editing uses the title-only API and preserves draft across switches and polling',async({page})=>{
  const {notes,requests}=await setup(page);const rich=notes.find(n=>n.id==='rich');rich.titleEditable=true;
  await openNote(page,/Tablet çizimleri/);await expect(page.getByLabel('Not başlığı')).toBeEditable();await expect(page.getByLabel('Not içeriği')).toHaveAttribute('readonly','');
  await page.getByLabel('Not başlığı').fill('Düzenlenen kitap başlığı');await openNote(page,/Aklımda kalanlar/);await openNote(page,/Düzenlenen kitap başlığı/);
  await page.waitForResponse(response=>response.url().endsWith('/api/notes/rich')&&response.request().method()==='GET');
  await expect(page.getByLabel('Not başlığı')).toHaveValue('Düzenlenen kitap başlığı');
  // Ctrl+S only acts once the reopened note's details are loaded (the Save button is enabled then).
  await expect(page.getByRole('button',{name:'Kaydet',exact:true})).toBeEnabled();await page.keyboard.press('ControlOrMeta+s');
  await expect(page.getByText('Kaydedilmedi',{exact:true})).toHaveCount(0);
  // The save is asynchronous after Ctrl+S: wait for the request instead of reading the log at once.
  await expect.poll(()=>requests.filter(r=>r.method==='PATCH'&&r.path.endsWith('/title')).map(r=>r.data)).toEqual([{title:'Düzenlenen kitap başlığı',expectedRevision:1}]);
  expect(requests.some(r=>r.method==='PUT'&&r.path.endsWith('/rich'))).toBe(false);expect(rich.body).toBe('Çizim notunun metin önizlemesi.');
  await page.reload();await openNote(page,/Düzenlenen kitap başlığı/);await expect(page.getByLabel('Not başlığı')).toHaveValue('Düzenlenen kitap başlığı');await expect(page.getByLabel('Not içeriği')).toHaveAttribute('readonly','');
});

test('real rich editor selection, formatting, lists, history and revisioned API preserve the Samsung model',async({page})=>{
  const {notes}=await setup(page);const note=notes.find(n=>n.id==='one');note.body='Merhaba 🌱\nİkinci satır';
  let model:any={schemaVersion:1,text:note.body,spans:[],paragraphs:[]};const writes:any[]=[];let unavailable=false;
  await page.route('**/api/notes/one/editor',async route=>{
    if(route.request().method()==='GET'){if(unavailable)return route.fulfill({status:503,json:{error:{message:'Sentetik geçici hata'}}});return route.fulfill({json:{schemaVersion:1,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false},model,reason:null,revision:note.revision}});}
    const data=route.request().postDataJSON();writes.push(data);if(data.expectedRevision!==note.revision)return route.fulfill({status:409,json:{error:{code:'revision_conflict',message:'Sürüm değişti'}}});model=data.model;note.body=model.text;note.title=data.title;note.revision++;note.syncState='pending';return route.fulfill({json:{note}});
  });
  await openNote(page,/Aklımda kalanlar/);const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toHaveAttribute('contenteditable','true');
  await editor.locator('p').first().evaluate(element=>{const node=element.firstChild!;const range=document.createRange();range.setStart(node,0);range.setEnd(node,7);const selection=window.getSelection()!;selection.removeAllRanges();selection.addRange(range);});
  await page.getByRole('button',{name:'Kalın',exact:true}).click();await expect(page.getByRole('button',{name:'Kalın',exact:true})).toHaveAttribute('aria-pressed','true');
  await page.getByRole('button',{name:'İtalik',exact:true}).click();await page.getByRole('button',{name:'Yazı rengi 1',exact:true}).click();await page.getByRole('button',{name:'Vurgu rengi 2',exact:true}).click();
  await page.getByLabel('Yazı boyutu').selectOption('17');await page.getByLabel('Yazı boyutu').selectOption('');await expect(editor.locator('[data-samsung-kind="fontSize"]')).toHaveCount(0);await page.getByLabel('Yazı boyutu').selectOption('17');await page.getByRole('button',{name:'Ortala',exact:true}).click();await page.getByRole('button',{name:'Onay kutusu',exact:true}).click();await page.getByRole('button',{name:'Girintiyi artır',exact:true}).click();
  await expect(editor.locator('p').first()).toHaveAttribute('data-native-bullet','check');await expect(editor.locator('p').first()).toHaveCSS('text-align','center');
  await page.getByRole('button',{name:'Geri al',exact:true}).click();await page.getByRole('button',{name:'Yinele',exact:true}).click();
  await page.keyboard.press('ControlOrMeta+s');await expect(page.getByText('Kaydedilmedi',{exact:true})).toHaveCount(0);expect(writes).toHaveLength(1);expect(writes[0]).not.toHaveProperty('body');expect(writes[0].expectedRevision).toBe(1);
  expect(model.text).toBe('Merhaba 🌱\nİkinci satır');expect(model.spans).toContainEqual({start:0,end:7,kind:'bold',value:true,interval:3});expect(model.spans).toContainEqual({start:0,end:7,kind:'foregroundColor',value:'#FFFF343B',interval:3});expect(model.spans).toContainEqual({start:0,end:7,kind:'fontSize',value:17,interval:3});expect(model.paragraphs[0]).toMatchObject({index:0,alignment:'center',bullet:{kind:'check',checked:false},indentLevel:{level:1,direction:'ltr'}});
  await editor.press('ControlOrMeta+End');await page.screenshot({path:'/tmp/notes-rich-editor.png'});await page.reload();await openNote(page,/Aklımda kalanlar/);await expect(page.locator('.rich-document-body span[data-samsung-kind="bold"]')).toHaveText('Merhaba');await expect(page.locator('.rich-document-body p').first()).toHaveAttribute('data-native-bullet','check');
  const beforeZoom=await editor.locator('[data-samsung-kind="fontSize"]').first().boundingBox();await page.getByRole('button',{name:'Yakınlaştır',exact:true}).click();await expect.poll(async()=>{const box=await editor.locator('[data-samsung-kind="fontSize"]').first().boundingBox();return box!.width/beforeZoom!.width;}).toBeGreaterThan(1.09);await page.getByRole('button',{name:'Uzaklaştır',exact:true}).click();
  await page.getByRole('checkbox',{name:'Yapılacak öğesini tamamla'}).click();await expect(page.getByRole('checkbox')).toHaveAttribute('aria-checked','true');
  await editor.click();await page.keyboard.press('ControlOrMeta+End');await page.keyboard.press('Enter');await page.keyboard.type('Yeni satır');await expect(editor).toHaveAttribute('contenteditable','true');
  await page.keyboard.press('ControlOrMeta+s');await expect(page.getByText('Kaydedilmedi',{exact:true})).toHaveCount(0);expect(model.paragraphs[0].bullet.checked).toBe(true);expect(model.text).toBe('Merhaba 🌱\nİkinci satır\nYeni satır');
  await editor.click();await page.keyboard.press('ControlOrMeta+End');await page.keyboard.type(' taslak');unavailable=true;
  await page.waitForResponse(response=>response.url().endsWith('/api/notes/one/editor')&&response.status()===503);await expect(editor).toHaveAttribute('contenteditable','true');await expect(editor).toContainText('Yeni satır taslak');
  unavailable=false;await page.keyboard.press('ControlOrMeta+s');await expect(page.getByText('Kaydedilmedi',{exact:true})).toHaveCount(0);expect(model.text).toContain('Yeni satır taslak');expect(writes).toHaveLength(3);
});
test('a picture anchored in the text (objectSpan) stays in place while text around it is typed and formatted',async({page})=>{
  const {notes}=await setup(page);const note=notes.find(n=>n.id==='one');note.body='Önce\n\uFFFC\nSonra';
  const picture='0e5d4a3c-4444-4c1e-9a55-0a4fbb2f7a10';
  let model:any={schemaVersion:1,text:note.body,spans:[],paragraphs:[],objects:[{start:5,objectId:picture,type:'image'}]};const writes:any[]=[];
  await page.route('**/api/notes/one/editor',async route=>{
    if(route.request().method()==='GET')return route.fulfill({json:{schemaVersion:1,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false},model,reason:null,revision:note.revision}});
    const data=route.request().postDataJSON();writes.push(data);model=data.model;note.body=model.text;note.revision++;note.syncState='pending';return route.fulfill({json:{note}});
  });
  await openNote(page,/Aklımda kalanlar/);const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toHaveAttribute('contenteditable','true');
  const chip=editor.locator('.native-object-anchor');await expect(chip).toHaveCount(1);await expect(chip).toHaveText('Resim');await expect(chip).toHaveAttribute('contenteditable','false');await expect(chip).toHaveCSS('animation-name','none');
  // Select everything and delete / overtype: the anchor cannot go, so nothing changes.
  await editor.click();await page.keyboard.press('ControlOrMeta+a');await page.keyboard.press('Delete');await page.keyboard.press('ControlOrMeta+a');await page.keyboard.type('x');
  await expect(chip).toHaveCount(1);await expect(editor).toContainText('Önce');await expect(editor).toContainText('Sonra');
  // Type before and after the picture, then bold the first word.
  await page.keyboard.press('ControlOrMeta+Home');await page.keyboard.press('End');await page.keyboard.type(' yeni');
  await page.keyboard.press('ControlOrMeta+End');await page.keyboard.type(' son');
  await editor.locator('p').first().evaluate(element=>{const node=element.firstChild!;const range=document.createRange();range.setStart(node,0);range.setEnd(node,4);const selection=window.getSelection()!;selection.removeAllRanges();selection.addRange(range);});
  await page.getByRole('button',{name:'Kalın',exact:true}).click();
  await page.keyboard.press('ControlOrMeta+s');await expect(page.getByText('Kaydedilmedi',{exact:true})).toHaveCount(0);expect(writes).toHaveLength(1);
  expect(model.text).toBe('Önce yeni\n\uFFFC\nSonra son');expect(model.objects).toEqual([{start:10,objectId:picture,type:'image'}]);expect(model.spans).toContainEqual({start:0,end:4,kind:'bold',value:true,interval:3});
  // Pasted text never creates an anchor.
  await editor.click();await page.keyboard.press('ControlOrMeta+End');
  await editor.evaluate(element=>{const data=new DataTransfer();data.setData('text/plain','a\uFFFCb');element.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true}));});
  await expect(chip).toHaveCount(1);await expect(editor).toContainText('Sonra sonab');
});
test('unsupported native typing anchors keep original rich content read-only instead of flattening it',async({page})=>{
  const {notes,requests}=await setup(page);const note=notes.find(n=>n.id==='rich');note.titleEditable=true;
  await page.route('**/api/notes/rich/editor',route=>route.fulfill({json:{schemaVersion:1,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false},model:{schemaVersion:1,text:note.body,spans:[{start:0,end:0,kind:'bold',value:false,interval:3}],paragraphs:[]},reason:null,revision:note.revision}}));
  await openNote(page,/Tablet çizimleri/);await expect(page.getByText('Bu notun sayfaları burada gösterilemiyor; yalnızca metni görünüyor.',{exact:true})).toBeVisible();await expect(page.getByLabel('Not içeriği')).toHaveAttribute('readonly','');await expect(page.getByLabel('Not başlığı')).toBeEditable();await expect(page.getByRole('button',{name:'Kalın',exact:true})).toHaveCount(0);await expect(page.getByText('Bu notta yazı biçimlendirme şimdilik kullanılamıyor.',{exact:true})).toBeVisible();expect(requests.filter(r=>r.method==='PUT')).toHaveLength(0);
});

test('numbered selections continue on Enter and completed checklists create unchecked items',async({page})=>{
  const errors:string[]=[];page.on('pageerror',error=>errors.push(error.message));
  const {notes}=await setup(page);const note=notes.find(n=>n.id==='one');note.body='Bir\nİki';
  await page.route('**/api/notes/one/editor',route=>route.fulfill({json:{schemaVersion:1,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false},model:{schemaVersion:1,text:note.body,spans:[],paragraphs:[]},reason:null,revision:1}}));
  await openNote(page,/Aklımda kalanlar/);const editor=page.getByLabel('Not içeriği');await expect(editor).toHaveAttribute('contenteditable','true');await editor.click();await page.keyboard.press('ControlOrMeta+a');await page.getByRole('button',{name:'Numaralı liste',exact:true}).click();
  await expect(editor.locator('p').nth(0)).toHaveAttribute('data-native-number','1');await expect(editor.locator('p').nth(1)).toHaveAttribute('data-native-number','2');await expect(editor).toBeFocused();
  await page.keyboard.press('ControlOrMeta+End');await page.keyboard.press('Enter');await page.keyboard.type('Üç');expect(errors).toEqual([]);await expect(editor.locator('p').nth(2)).toHaveAttribute('data-native-number','3');
  await page.getByRole('button',{name:'Onay kutusu',exact:true}).click();await page.getByRole('checkbox').click();await expect(page.getByRole('checkbox')).toHaveAttribute('aria-checked','true');
  await editor.click();await page.keyboard.press('ControlOrMeta+End');await page.keyboard.press('Enter');await expect(editor.locator('p').nth(3)).toHaveAttribute('data-native-checked','false');await expect(editor).toHaveAttribute('contenteditable','true');
  await page.keyboard.press('Enter');await expect(editor.locator('p')).toHaveCount(4);await expect(editor.locator('p').nth(3)).toHaveAttribute('data-native-bullet','none');await page.keyboard.type('Normal metin');await expect(editor.locator('p').nth(3)).toHaveText('Normal metin');
});

test('visible real page thumbnails use authorized bounded requests and release URLs off screen',async({page})=>{
  const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64');
  let active=0,maximum=0,gate=true;const release:(()=>void)[]=[];const requested:string[]=[];
  await page.addInitScript(()=>{const original=URL.revokeObjectURL;(window as any).__thumbnailRevoked=[];URL.revokeObjectURL=url=>{(window as any).__thumbnailRevoked.push(url);original(url);};});
  await setup(page,async notes=>{
    notes.splice(0,notes.length,...Array.from({length:80},(_,index)=>({...base,id:`thumbnail-${index}`,title:index===0?'Aklımda kalanlar 00':`Sentetik not ${String(index).padStart(2,'0')}`,body:'Sentetik not',preview:'Sentetik not'})));
    await page.route('**/api/notes/*/thumbnail?*',async route=>{expect(route.request().headers()['authorization']).toBe(`Bearer ${TOKEN}`);expect(new URL(route.request().url()).searchParams.get('revision')).toBe('1');active++;maximum=Math.max(maximum,active);requested.push(new URL(route.request().url()).pathname.split('/')[3]);if(gate)await new Promise<void>(resolve=>release.push(resolve));active--;await route.fulfill({contentType:'image/png',body:png,headers:{'X-Notes-Preview-Complete':'true'}});});
  });
  await expect.poll(()=>requested.length).toBe(3);expect(maximum).toBe(3);expect(requested).not.toContain('thumbnail-79');gate=false;release.splice(0).forEach(resolve=>resolve());
  await expect(page.locator('.thumbnail-image').first()).toBeVisible();const first=await page.locator('.thumbnail-image').first().getAttribute('src');expect(first).toMatch(/^blob:/);
  expect(requested.length).toBeLessThan(80);await page.locator('.note-rows').evaluate(element=>{element.scrollTop=element.scrollHeight;});
  await expect.poll(()=>requested.includes('thumbnail-79')).toBe(true);await expect.poll(()=>page.evaluate(()=> (window as any).__thumbnailRevoked)).toContain(first);expect(maximum).toBeLessThanOrEqual(3);
  await page.getByRole('button',{name:/Sentetik not 79/}).scrollIntoViewIfNeeded();await expect(page.getByRole('button',{name:/Sentetik not 79/}).locator('.thumbnail-image')).toBeVisible();
});

test('thumbnail pending, terminal failures and revision changes preserve honest fallbacks',async({page})=>{
  const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64');const counts:Record<string,number>={};const revisions:string[]=[];
  const {notes}=await setup(page,async()=>{
    await page.route('**/api/notes/*/thumbnail?*',route=>{const url=new URL(route.request().url()),id=url.pathname.split('/')[3];counts[id]=(counts[id]||0)+1;if(id==='one'){revisions.push(url.searchParams.get('revision')!);if(counts[id]===1)return route.fulfill({status:202,json:{state:'pending'},headers:{'Retry-After':'1'}});return route.fulfill({contentType:'image/png',body:png,headers:{'X-Notes-Preview-Complete':'false'}});}return route.fulfill({status:id==='rich'?503:404,json:{error:{code:'thumbnail_unavailable',message:'Sentetik önizleme yok'}}});});
    await page.route('**/api/sync',route=>route.fulfill({json:{state:'queued'}}));
  });
  const first=page.getByRole('button',{name:/Aklımda kalanlar/});await expect(first.locator('.thumbnail-image')).toBeVisible();await expect(first.getByText('Kısmi önizleme')).toBeVisible();
  const second=page.getByRole('button',{name:/Hafta sonu için/});await expect(second.locator('.thumbnail-text')).toHaveText('Kitapçıya uğra. Yeni bir yürüyüş rotası bul.');expect(counts.two).toBe(1);
  await page.waitForResponse(response=>response.url().endsWith('/api/status'));expect(counts.two).toBe(1);
  notes.find(note=>note.id==='one').revision=2;await expect.poll(()=>revisions.includes('2'),{timeout:7000}).toBe(true);await expect(first.locator('.thumbnail-image')).toBeVisible();expect(counts.two).toBe(1);
  await page.getByRole('button',{name:'Ayarlar',exact:true}).click();await page.getByRole('button',{name:'Şimdi eşitle',exact:true}).click();await expect.poll(()=>counts.two).toBe(2);
});

test('pending thumbnails retain all three work slots instead of filling the backend queue',async({page})=>{
  const requested:string[]=[];
  await setup(page,async notes=>{
    notes.splice(0,notes.length,...Array.from({length:40},(_,index)=>({...base,id:`pending-${index}`,title:index===0?'Aklımda kalanlar':`Bekleyen not ${index}`,body:'Sentetik',preview:'Sentetik'})));
    await page.route('**/api/notes/*/thumbnail?*',route=>{requested.push(new URL(route.request().url()).pathname.split('/')[3]);return route.fulfill({status:202,json:{state:'pending'},headers:{'Retry-After':'1'}});});
  });
  await expect.poll(()=>requested.filter(id=>id==='pending-0').length).toBeGreaterThanOrEqual(2);
  expect(new Set(requested).size).toBe(3);
});

test('device preferences hydrate before writes and serialize the latest user intent',async({page})=>{
  let releaseRead:()=>void=()=>{};let releaseWrite:()=>void=()=>{};const writes:any[]=[];
  let saved={version:1,galleryMode:'grid',sortAscending:false,toolsOpen:false,defaultZoom:75};
  await setup(page,async()=>{
    await page.route('**/api/preferences',async route=>{
      if(route.request().method()==='GET'){await new Promise<void>(resolve=>{releaseRead=resolve;});return route.fulfill({json:saved});}
      const data=route.request().postDataJSON();writes.push(data);if(writes.length===1)await new Promise<void>(resolve=>{releaseWrite=resolve;});saved=data;return route.fulfill({json:saved});
    });
  });
  await page.getByRole('button',{name:'Ayarlar',exact:true}).click();await page.getByRole('button',{name:'Liste',exact:true}).click();await page.getByLabel('Varsayılan yakınlaştırma').selectOption('125');expect(writes).toHaveLength(0);
  releaseRead();await expect.poll(()=>writes.length).toBe(1);expect(writes[0]).toEqual({version:1,galleryMode:'list',sortAscending:false,toolsOpen:false,defaultZoom:125});
  await page.getByLabel('Varsayılan yakınlaştırma').selectOption('150');expect(writes).toHaveLength(1);releaseWrite();await expect.poll(()=>writes.length).toBe(2);expect(writes[1]).toEqual({...writes[0],defaultZoom:150});
  await page.getByRole('button',{name:'Ayarları kapat'}).click();await expect(page.locator('.note-rows')).toHaveClass(/gallery-list/);
});

test('failed device preference save remains explicit and retries only on user action',async({page})=>{
  let writes=0;await setup(page,async()=>{
    await page.route('**/api/preferences',route=>{
      if(route.request().method()==='GET')return route.fulfill({json:{version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100}});
      writes++;return writes===1?route.fulfill({status:503,json:{error:{code:'synthetic_preferences_failure',message:'Sentetik kayıt hatası'}}}):route.fulfill({json:route.request().postDataJSON()});
    });
  });
  await page.getByRole('button',{name:'Ayarlar',exact:true}).click();await page.getByRole('button',{name:'Liste',exact:true}).click();await expect(page.getByText(/Tercihler bu oturumda uygulanıyor, ancak cihazda saklanamadı/)).toBeVisible();
  await page.waitForResponse(response=>response.url().endsWith('/api/status'));expect(writes).toBe(1);
  await page.getByRole('button',{name:'Liste',exact:true}).click();await expect.poll(()=>writes).toBe(2);await expect(page.getByText(/Tercihler bu oturumda uygulanıyor, ancak cihazda saklanamadı/)).toHaveCount(0);
});

test('folder right click replaces ellipsis and supports keyboard, pointer placement and dismissal',async({page})=>{
  const {requests}=await setup(page);const folder=page.locator('.folder-item').filter({hasText:'Çalışma'});
  await expect(page.locator('.folder-more')).toHaveCount(0);await folder.focus();await page.keyboard.press('Shift+F10');
  const menu=page.getByRole('menu',{name:'Klasör menüsü'});await expect(menu).toBeVisible();await expect(menu.getByRole('menuitem',{name:'Yeniden adlandır'})).toBeFocused();
  await page.keyboard.press('ArrowDown');await expect(menu.getByRole('menuitem',{name:'Klasörü taşı'})).toBeFocused();await page.keyboard.press('Escape');await expect(menu).toHaveCount(0);await expect(folder).toBeFocused();
  await folder.press('ContextMenu');await expect(menu).toBeVisible();await page.locator('.list-title h1').click();await expect(menu).toHaveCount(0);
  await folder.click({button:'right'});await expect(menu).toBeVisible();await page.locator('.note-rows').dispatchEvent('scroll');await expect(menu).toHaveCount(0);
  const viewport=page.viewportSize()!;await folder.evaluate((element,point)=>element.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,clientX:point.x,clientY:point.y})),{x:viewport.width-1,y:viewport.height-1});await expect(menu).toBeVisible();await menu.evaluate(element=>Promise.all(element.getAnimations().map(animation=>animation.finished)));
  const box=await menu.boundingBox();expect(box!.x).toBeGreaterThanOrEqual(0);expect(box!.y).toBeGreaterThanOrEqual(0);expect(box!.x+box!.width).toBeLessThanOrEqual(viewport.width);expect(box!.y+box!.height).toBeLessThanOrEqual(viewport.height);
  expect(requests.filter(request=>['POST','PATCH','PUT','DELETE'].includes(request.method))).toHaveLength(0);
  await page.screenshot({path:'/tmp/notes-context-folder.png'});
});

test('note right click acts on clicked record without opening it or losing another note draft',async({page})=>{
  const {requests}=await setup(page,async notes=>{notes.find(note=>note.id==='one').folderId='work';notes.find(note=>note.id==='two').folderId='ideas';});
  await openNote(page,/Aklımda kalanlar/);await page.getByLabel('Not içeriği').fill('Başka notta korunacak taslak');await page.getByRole('button',{name:'Not listesine dön'}).click();
  const card=page.getByRole('button',{name:/Hafta sonu için/});await card.click({button:'right'});const menu=page.getByRole('menu',{name:'Not menüsü'});await expect(menu).toBeVisible();await expect(page.locator('.editor-panel')).toBeHidden();
  await page.keyboard.press('ControlOrMeta+s');await page.keyboard.press('ControlOrMeta+n');await expect(menu).toBeVisible();expect(requests.filter(request=>['PUT','POST','PATCH'].includes(request.method))).toHaveLength(0);
  expect(requests.filter(request=>request.path==='/api/notes/two'&&request.method==='GET')).toHaveLength(0);await expect(menu.getByRole('menuitem')).toHaveText(['Taşı','Paylaş','Sil','Dosya olarak kaydet','Notu çoğalt','Favorilerden çıkar']);await expect(menu.getByRole('menuitem',{name:/Kilitle/})).toHaveCount(0);await menu.getByRole('menuitem',{name:'Taşı',exact:true}).click();await expect(folderOption(page,'Hedef klasör','Fikirler')).toHaveAttribute('aria-current','location');await expect(folderOption(page,'Hedef klasör','Fikirler')).toHaveAttribute('aria-disabled','true');await page.getByRole('button',{name:'Vazgeç',exact:true}).click();
  expect(requests.filter(request=>request.method==='PATCH')).toHaveLength(0);await card.click({button:'right'});await menu.getByRole('menuitem',{name:'Favorilerden çıkar'}).click();
  await expect.poll(()=>requests.filter(request=>request.method==='PATCH').length).toBe(1);expect(requests.find(request=>request.method==='PATCH')).toMatchObject({path:'/api/notes/two',data:{favorite:false,expectedRevision:1}});
  await openNote(page,/Aklımda kalanlar/);await expect(page.getByLabel('Not içeriği')).toHaveValue('Başka notta korunacak taslak');
});

test('note context selection applies only to the frozen selected IDs and respects pending gates',async({page})=>{
  const {requests,state}=await setup(page);state.autoConfirm=false;
  await page.getByRole('button',{name:'Düzenle',exact:true}).click();for(const title of [/Aklımda kalanlar/,/Hafta sonu için/])await page.getByRole('button',{name:title}).click();
  await expect(page.getByText('2 not seçildi')).toBeVisible();await page.getByRole('button',{name:/Aklımda kalanlar/}).click({button:'right'});await page.getByRole('menuitem',{name:'Favorilere ekle',exact:true}).click();
  await expect.poll(()=>requests.filter(request=>request.method==='PATCH').length).toBe(2);expect(requests.filter(request=>request.method==='PATCH').map(request=>request.path).sort()).toEqual(['/api/notes/one','/api/notes/two']);
  await page.getByRole('button',{name:/Aklımda kalanlar/}).click({button:'right'});await expect(page.getByRole('menuitem',{name:'Sil',exact:true})).toBeDisabled();await page.keyboard.press('Escape');expect(requests.filter(request=>request.method==='PATCH')).toHaveLength(2);
});
test('editor completes Samsung editing actions: justify, duplicate with formatting, share as clipboard text',async({page,context})=>{
  await context.grantPermissions(['clipboard-read','clipboard-write']);
  const {notes}=await setup(page);const note=notes.find(n=>n.id==='one');note.body='Merhaba 🌱\nİkinci satır';
  let model:any={schemaVersion:1,text:note.body,spans:[],paragraphs:[]};const writes:any[]=[];const created:any[]=[];
  await page.route('**/api/notes/one/editor',async route=>{
    if(route.request().method()==='GET')return route.fulfill({json:{schemaVersion:1,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false},model,reason:null,revision:note.revision}});
    const data=route.request().postDataJSON();writes.push(data);model=data.model;note.body=model.text;note.revision++;return route.fulfill({json:{note}});
  });
  await page.route('**/api/notes/editor',async route=>{const data=route.request().postDataJSON();created.push(data);const copy={...note,id:'copy',title:data.title,revision:1};notes.push(copy);return route.fulfill({status:201,json:{note:copy}});});
  await openNote(page,/Aklımda kalanlar/);const editor=page.getByRole('textbox',{name:'Not içeriği'});await expect(editor).toHaveAttribute('contenteditable','true');
  await editor.locator('p').first().click();await page.getByRole('button',{name:'İki yana yasla',exact:true}).click();
  await expect(editor.locator('p').first()).toHaveCSS('text-align','justify');await expect(page.getByRole('button',{name:'İki yana yasla',exact:true})).toHaveAttribute('aria-pressed','true');
  await page.keyboard.press('ControlOrMeta+s');await expect.poll(()=>writes.length).toBe(1);expect(writes[0].model.paragraphs[0]).toMatchObject({index:0,alignment:'justify'});
  await page.getByRole('button',{name:'Paylaş'}).click();await page.getByRole('button',{name:'Metni panoya kopyala',exact:true}).click();
  await expect(page.getByRole('button',{name:'Metni panoya kopyala',exact:true})).toHaveCount(0);
  expect(await page.evaluate(()=>navigator.clipboard.readText())).toBe('Aklımda kalanlar\n\nMerhaba 🌱\nİkinci satır');
  await page.getByRole('button',{name:'Not seçenekleri'}).click();await page.getByRole('button',{name:'Kopyasını oluştur',exact:true}).click();
  await expect.poll(()=>created.length).toBe(1);expect(created[0].title).toBe('Aklımda kalanlar — kopya');expect(created[0].model.paragraphs[0].alignment).toBe('justify');expect(created[0].model.text).toBe('Merhaba 🌱\nİkinci satır');
});

test('page settings paints every page with a page background colour through one revisioned request',async({page})=>{
  const {notes}=await setup(page);const note=notes.find(n=>n.id==='one');const writes:any[]=[];
  const palette=[[{argb:0xFFFCFCFC,name:'Renk yok'},{argb:0xFFF5DDDD,name:'Pudra pembe'}],[{argb:0xFFC9E0F9,name:'Bulut mavisi'}]];
  let state:any={revision:note.revision,backgroundArgb:0xFFFCFCFC,editable:true,reason:null,palette};
  await page.route('**/api/notes/one/page-style',async route=>{
    if(route.request().method()==='GET')return route.fulfill({json:state});
    const data=route.request().postDataJSON();writes.push(data);note.revision++;note.syncState='pending';
    state={...state,revision:note.revision,backgroundArgb:data.backgroundArgb,pending:true,editable:false,reason:'Önce bu notun bekleyen eşitlemesi tamamlanmalı.'};
    return route.fulfill({json:{note}});
  });
  await openNote(page,/Aklımda kalanlar/);
  await page.getByRole('button',{name:'Sayfa ayarları'}).click();
  await expect(page.getByRole('button',{name:'Renk yok'})).toHaveAttribute('aria-pressed','true');
  await expect(page.getByRole('button',{name:'Renk yok'})).toBeDisabled();
  const revision=note.revision;await page.getByRole('button',{name:'Bulut mavisi'}).click();
  await expect.poll(()=>writes.length).toBe(1);expect(writes[0]).toEqual({backgroundArgb:0xFFC9E0F9,expectedRevision:revision});
  await expect(page.getByRole('dialog',{name:'Sayfa ayarları'})).toHaveCount(0);
  await page.getByRole('button',{name:'Sayfa ayarları'}).click();
  await expect(page.getByText('Renk değişikliği eşitleniyor.')).toBeVisible();
  await expect(page.getByRole('button',{name:'Pudra pembe'})).toBeDisabled();
  await page.keyboard.press('Escape');await expect(page.getByRole('dialog',{name:'Sayfa ayarları'})).toHaveCount(0);
  expect(writes.length).toBe(1);
});
test('trash shows Samsung days left, folder cards first and only real restore actions',async({page})=>{
  const day=86400000,now=Date.now();
  const {requests}=await setup(page,async notes=>{const note=notes.find(n=>n.id==='two');note.trashed=true;note.trashedAt=now-3*day-1000;});
  await page.getByRole('button',{name:'Çöp kutusu',exact:true}).click();
  await expect(page.getByText('Öğelerde, kalıcı olarak silinmelerine kalan gün sayısı gösterilir.')).toBeVisible();
  const card=page.getByRole('button',{name:/Hafta sonu için/});await expect(card.locator('.thumbnail-band')).toHaveText('27 gün');
  await card.click({button:'right'});const menu=page.getByRole('menu',{name:'Not menüsü'});
  await expect(menu.getByRole('menuitem')).toHaveText(['Geri yükle']);await page.keyboard.press('Escape');
  await expect(page.getByRole('button',{name:/Boşalt|Kalıcı/})).toHaveCount(0);
  expect(requests.filter(r=>['PATCH','POST','PUT','DELETE'].includes(r.method))).toHaveLength(0);
  await page.screenshot({path:'/tmp/notes-trash-parity.png'});
});

test('dropdowns close on a press outside them and on Escape',async({page})=>{
  await setup(page);
  await page.getByRole('button',{name:'Görünüm'}).click();await expect(page.locator('.view-menu')).toBeVisible();
  await page.locator('.list-title').click();await expect(page.locator('.view-menu')).toHaveCount(0);
  await openNote(page,/Aklımda kalanlar/);
  await page.getByRole('button',{name:'Not seçenekleri'}).click();await expect(page.locator('.note-menu')).toBeVisible();
  await page.getByLabel('Not başlığı').click();await expect(page.locator('.note-menu')).toHaveCount(0);
  await page.getByRole('button',{name:'Paylaş'}).click();await expect(page.locator('.share-menu')).toBeVisible();
  await page.keyboard.press('Escape');await expect(page.locator('.share-menu')).toHaveCount(0);
  // Another dropdown's toggle switches menus instead of stacking them.
  await page.getByRole('button',{name:'Paylaş'}).click();await page.getByRole('button',{name:'Not seçenekleri'}).click();
  await expect(page.locator('.share-menu')).toHaveCount(0);await expect(page.locator('.note-menu')).toBeVisible();
});

// Move dialog folder picker and press-and-hold drag (FolderPicker.tsx, useNoteDrag.tsx).
const shot=(name:string)=>process.env.NOTES_SHOT_DIR?path.join(process.env.NOTES_SHOT_DIR,name):undefined;
const center=async(locator:import('@playwright/test').Locator)=>{const box=(await locator.boundingBox())!;return {x:box.x+box.width/2,y:box.y+box.height/2};};
async function holdOn(page:Page,locator:import('@playwright/test').Locator){const p=await center(locator);await page.mouse.move(p.x,p.y);await page.mouse.down();await page.waitForTimeout(470);}
async function moveOver(page:Page,locator:import('@playwright/test').Locator){const p=await center(locator);await page.mouse.move(p.x,p.y,{steps:6});}
const sidebarFolder=(page:Page,name:string)=>page.locator('.folder-item').filter({hasText:name});
const patches=(requests:{method:string;path:string;data:any}[])=>requests.filter(r=>r.method==='PATCH');

test('move dialog uses a light folder listbox with keyboard, type-ahead and mouse selection',async({page})=>{
  const {requests,folders,state,notes}=await setup(page);state.autoConfirm=false;
  folders.push({state:'normal',syncState:'synced',error:null,id:'memo',name:'Ekran kapalı notları',parentId:null,color:null,noteCount:0,system:true},{state:'normal',syncState:'pending',error:null,id:'later',name:'Bekleyen klasör',parentId:null,color:null,noteCount:0});
  notes.push({...base,id:'memo-note',folderId:'memo',title:'Kısa not',body:'x',preview:'x'});
  await page.reload();await expect(page.getByRole('button',{name:/Aklımda kalanlar/})).toBeVisible();
  await page.getByRole('button',{name:/Aklımda kalanlar/}).click({button:'right'});await page.getByRole('menuitem',{name:'Taşı',exact:true}).click();
  const dialog=page.getByRole('dialog',{name:'Klasöre taşı'}),list=dialog.getByRole('listbox',{name:'Hedef klasör'});
  await expect(dialog.locator('select')).toHaveCount(0);await expect(list).toBeFocused();
  await expect(list.getByRole('option')).toHaveCount(5);
  // Klasörsüz first and current; the system and the unsynced folder explain why they are unavailable.
  const root=folderOption(page,'Hedef klasör','Klasörsüz');
  await expect(list.getByRole('option').first()).toHaveAttribute('data-folder-id','');
  await expect(root).toHaveAttribute('aria-current','location');await expect(root).toHaveAttribute('aria-disabled','true');await expect(root).toHaveAttribute('title','Not zaten burada.');
  await expect(folderOption(page,'Hedef klasör','Ekran kapalı')).toHaveAttribute('title','Sistem klasörlerine not taşınamaz.');
  await expect(folderOption(page,'Hedef klasör','Bekleyen klasör')).toHaveAttribute('title','Klasörün eşitlemesi tamamlanmalı.');
  await expect(dialog.getByRole('button',{name:'Taşı',exact:true})).toBeDisabled();
  // Child folders are indented under their parent.
  const indent=async(name:string)=>folderOption(page,'Hedef klasör',name).evaluate(e=>parseFloat(getComputedStyle(e).paddingInlineStart));
  expect(await indent('Fikirler')).toBeGreaterThan(await indent('Çalışma'));
  const selectedName=()=>list.locator('[aria-selected="true"]').getAttribute('data-folder-id');
  await page.keyboard.press('ArrowDown');expect(await selectedName()).toBe('work');
  await expect(list).toHaveAttribute('aria-activedescendant',(await folderOption(page,'Hedef klasör','Çalışma').getAttribute('id'))!);
  await page.keyboard.press('ArrowDown');expect(await selectedName()).toBe('ideas');
  await page.keyboard.press('ArrowDown');expect(await selectedName()).toBe('ideas');// disabled rows are skipped
  await page.keyboard.press('Home');expect(await selectedName()).toBe('work');
  await page.keyboard.press('End');expect(await selectedName()).toBe('ideas');
  await list.dispatchEvent('keydown',{key:'ç',bubbles:true});expect(await selectedName()).toBe('work');
  await page.waitForTimeout(750);await page.keyboard.press('f');expect(await selectedName()).toBe('ideas');
  await expect(folderOption(page,'Hedef klasör','Fikirler')).toHaveCSS('outline-style','solid');// focus ring only while navigating by keyboard
  await expect(folderOption(page,'Hedef klasör','Fikirler')).toHaveCSS('background-color','rgb(253, 236, 232)');
  if(shot('folder-picker.png'))await dialog.screenshot({path:shot('folder-picker.png')});
  await page.keyboard.press('ArrowUp');await page.keyboard.press('Enter');
  await expect(dialog).toHaveCount(0);
  await expect.poll(()=>patches(requests).length).toBe(1);expect(patches(requests)[0]).toMatchObject({path:'/api/notes/one',data:{folderId:'work',expectedRevision:1}});
  // Mouse: a click selects (no keyboard ring), Taşı moves; Escape closes without writing.
  await page.getByRole('button',{name:/Hafta sonu için/}).click({button:'right'});await page.getByRole('menuitem',{name:'Taşı',exact:true}).click();
  await folderOption(page,'Hedef klasör','Fikirler').click();await expect(folderOption(page,'Hedef klasör','Fikirler')).toHaveAttribute('aria-selected','true');
  await expect(folderOption(page,'Hedef klasör','Fikirler')).toHaveCSS('outline-style','none');
  await folderOption(page,'Hedef klasör','Ekran kapalı').click({force:true});await expect(folderOption(page,'Hedef klasör','Fikirler')).toHaveAttribute('aria-selected','true');
  await page.keyboard.press('Escape');await expect(dialog).toHaveCount(0);expect(patches(requests)).toHaveLength(1);
  await page.getByRole('button',{name:/Hafta sonu için/}).click({button:'right'});await page.getByRole('menuitem',{name:'Taşı',exact:true}).click();
  await folderOption(page,'Hedef klasör','Fikirler').click();await dialog.getByRole('button',{name:'Taşı',exact:true}).click();
  await expect.poll(()=>patches(requests).length).toBe(2);expect(patches(requests)[1]).toMatchObject({path:'/api/notes/two',data:{folderId:'ideas',expectedRevision:1}});
});

test('press and hold drags a note card onto a sidebar folder through the same move request',async({page})=>{
  const {requests,state}=await setup(page);state.autoConfirm=false;
  const card=page.getByRole('button',{name:/Aklımda kalanlar/});
  await holdOn(page,card);
  const ghost=page.getByTestId('note-drag-ghost');
  await expect(ghost).toBeVisible();await expect(ghost).toContainText('Aklımda kalanlar');await expect(ghost).toContainText('Bir klasörün üzerine bırakın');
  await expect(ghost).toHaveCSS('animation-name','none');await expect(ghost).toHaveCSS('transition-duration','0s');
  await expect(card).toHaveClass(/drag-source/);
  // Hovering a collapsed parent opens it; the child becomes a drop target.
  await moveOver(page,sidebarFolder(page,'Çalışma'));
  await expect(sidebarFolder(page,'Çalışma')).toHaveClass(/drop-target/);await expect(ghost).toContainText('Çalışma klasörüne taşı');
  await expect(sidebarFolder(page,'Fikirler')).toBeVisible({timeout:3000});
  await moveOver(page,sidebarFolder(page,'Fikirler'));
  await expect(sidebarFolder(page,'Fikirler')).toHaveClass(/drop-target/);await expect(sidebarFolder(page,'Çalışma')).not.toHaveClass(/drop-target/);
  if(shot('drag-ghost.png'))await page.screenshot({path:shot('drag-ghost.png'),clip:{x:0,y:0,width:760,height:420}});
  await page.mouse.up();
  await expect(ghost).toHaveCount(0);
  await expect.poll(()=>patches(requests).length).toBe(1);expect(patches(requests)[0]).toMatchObject({path:'/api/notes/one',data:{folderId:'ideas',expectedRevision:1}});
  await expect(page.getByRole('button',{name:'Not listesine dön'})).toBeHidden();// the drop did not open the note
  // A selection drags as "N not" and moves every selected note; "Tüm notlar" is not a target for notes already without a folder.
  await page.getByRole('button',{name:'Düzenle'}).click();
  await page.getByRole('button',{name:/Hafta sonu için/}).click();await page.getByRole('button',{name:/Tablet çizimleri/}).click();
  await holdOn(page,page.getByRole('button',{name:/Hafta sonu için/}));await expect(ghost).toContainText('2 not');
  await moveOver(page,page.locator('.nav-item').filter({hasText:'Tüm notlar'}));
  await expect(ghost).toContainText('Seçili notlar zaten burada.');await expect(page.locator('.nav-item').filter({hasText:'Tüm notlar'})).not.toHaveClass(/drop-target/);
  await moveOver(page,sidebarFolder(page,'Çalışma'));await expect(sidebarFolder(page,'Çalışma')).toHaveClass(/drop-target/);
  if(shot('drag-ghost-multi.png'))await page.screenshot({path:shot('drag-ghost-multi.png'),clip:{x:0,y:0,width:760,height:420}});
  await page.mouse.up();
  await expect.poll(()=>patches(requests).length).toBe(3);
  expect(patches(requests).slice(1).map(r=>[r.path,r.data])).toEqual([['/api/notes/two',{folderId:'work',expectedRevision:1}],['/api/notes/rich',{folderId:'work',expectedRevision:1}]]);
});

test('short click opens, right click menus, Escape and invalid folders cancel a drag',async({page})=>{
  const {requests,folders,state}=await setup(page);state.autoConfirm=false;
  folders.push({state:'normal',syncState:'pending',error:null,id:'later',name:'Bekleyen klasör',parentId:null,color:null,noteCount:0});
  await page.reload();const card=page.getByRole('button',{name:/Aklımda kalanlar/});await expect(card).toBeVisible();
  // Right click still opens the context menu and never starts a drag.
  await card.click({button:'right'});await expect(page.getByRole('menu',{name:'Not menüsü'})).toBeVisible();await expect(page.getByTestId('note-drag-ghost')).toHaveCount(0);
  await page.keyboard.press('Escape');await expect(page.getByRole('menu',{name:'Not menüsü'})).toHaveCount(0);
  // With a mouse, pressing and moving starts the drag at once (no need to hold still);
  // released away from any folder it writes nothing and does not open the note.
  let p=await center(card);await page.mouse.move(p.x,p.y);await page.mouse.down();await page.mouse.move(p.x+20,p.y+30,{steps:3});
  await expect(page.getByTestId('note-drag-ghost')).toBeVisible();await page.mouse.up();
  await expect(page.getByTestId('note-drag-ghost')).toHaveCount(0);await page.waitForTimeout(150);
  await expect(page.getByRole('button',{name:'Not listesine dön'})).toBeHidden();
  // Escape cancels: no write and the release does not open the note.
  await holdOn(page,card);await moveOver(page,sidebarFolder(page,'Çalışma'));await expect(sidebarFolder(page,'Çalışma')).toHaveClass(/drop-target/);
  await page.keyboard.press('Escape');await expect(page.getByTestId('note-drag-ghost')).toHaveCount(0);await expect(sidebarFolder(page,'Çalışma')).not.toHaveClass(/drop-target/);
  await page.mouse.up();await page.waitForTimeout(150);
  await expect(page.getByRole('button',{name:'Not listesine dön'})).toBeHidden();
  // An unsynced folder is not a drop target: no highlight, reason in the ghost, release writes nothing.
  await holdOn(page,card);await moveOver(page,sidebarFolder(page,'Bekleyen klasör'));
  await expect(page.getByTestId('note-drag-ghost')).toContainText('Klasörün eşitlemesi tamamlanmalı.');await expect(sidebarFolder(page,'Bekleyen klasör')).not.toHaveClass(/drop-target/);
  await page.mouse.up();await page.waitForTimeout(150);
  // Dropping elsewhere cancels too.
  await holdOn(page,card);await moveOver(page,page.locator('.list-title'));await page.mouse.up();
  expect(patches(requests)).toHaveLength(0);await expect(page.getByRole('button',{name:'Not listesine dön'})).toBeHidden();
  // A pen press-and-hold drags the same way (Pointer Events).
  p=await center(card);const target=await center(sidebarFolder(page,'Çalışma'));
  await card.dispatchEvent('pointerdown',{pointerType:'pen',pointerId:7,isPrimary:true,button:0,buttons:1,clientX:p.x,clientY:p.y,bubbles:true});
  await page.waitForTimeout(470);
  await page.evaluate(({x,y})=>{window.dispatchEvent(new PointerEvent('pointermove',{pointerType:'pen',pointerId:7,isPrimary:true,buttons:1,clientX:x,clientY:y,bubbles:true}));},target);
  await expect(sidebarFolder(page,'Çalışma')).toHaveClass(/drop-target/);
  await page.evaluate(({x,y})=>{window.dispatchEvent(new PointerEvent('pointerup',{pointerType:'pen',pointerId:7,isPrimary:true,clientX:x,clientY:y,bubbles:true}));},target);
  await expect.poll(()=>patches(requests).length).toBe(1);expect(patches(requests)[0]).toMatchObject({path:'/api/notes/one',data:{folderId:'work',expectedRevision:1}});
  // A short click still opens the note.
  await page.getByRole('button',{name:/Hafta sonu için/}).click();await expect(page.getByRole('button',{name:'Not listesine dön'})).toBeVisible();
});
