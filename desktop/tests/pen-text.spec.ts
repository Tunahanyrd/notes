import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// A pen note whose body text is editable (editor rules v2): it opens on its
// pages with the handwriting, "Metni düzenle" switches to the rich editor and
// a save goes through PUT /editor; the strokes are never part of that save.
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64');
async function fixture(page:Page){
  const requests:{path:string;method:string;data:any}[]=[];
  const note:any={id:'pen-text',title:'Kalem ve metin',folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',editable:false,titleEditable:true,readOnlyReason:'Çizim notu.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'Kalem notunun metni',preview:''};
  let model:any={schemaVersion:1,text:'Kalem notunun metni',spans:[],paragraphs:[]};
  const pages=[{id:'page-0',index:0,width:1000,height:1000,strokes:[{id:'source-0',kind:'fountain',colorARGB:0xff2462a0,size:4,points:[{x:10,y:20,pressure:.5,timeMs:0},{x:30,y:40,pressure:.6,timeMs:10}]}]}];
  await page.route('**/local-fonts/*',route=>route.fulfill({status:404,body:''}));
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method(),data=method==='GET'?null:route.request().postDataJSON();
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path.endsWith('/ink-draft'))return send({revision:null,strokes:[]});
    requests.push({path,method,data});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send({connected:true,accountLabel:'Sentetik hesap',loginPending:false,sync:{state:'idle',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,richTextEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,cloudDelete:false,attachments:false}});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes')return send({notes:[{...note,body:undefined}]});
    if(path.endsWith('/thumbnail')||path.endsWith('/preview'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/editor')&&method==='GET')return send({schemaVersion:1,revision:note.revision,model,reason:null,capabilities:{titleEdit:true,textEdit:true,richStyle:true,inkEdit:false}});
    if(path.endsWith('/editor')&&method==='PUT'){expect(data.expectedRevision).toBe(note.revision);model=data.model;Object.assign(note,{body:model.text,revision:note.revision+1});return send({note});}
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages:pages.map(item=>({...item,available:true,complete:true}))});
    if(path.endsWith('/ink'))return send({schemaVersion:1,revision:note.revision,pages,capabilities:{appendInk:true},reason:null,rendering:{nativeEquivalent:false,mode:'geometry-only'},pendingStrokes:[]});
    if(path.endsWith('/shapes'))return send({schemaVersion:1,revision:note.revision,pages:[]});
    if(method==='GET')return send({note});
    return send({error:{code:'unexpected_mutation',message:'Beklenmeyen değişiklik'}},422);
  });
  await page.goto('/#token='+TOKEN);await page.getByRole('button',{name:/Kalem ve metin/}).click();
  return {requests};
}
test('a pen note with editable text opens on its pages and edits its text in the text view',async({page})=>{
  const {requests}=await fixture(page);
  const switcher=page.getByRole('group',{name:'Görünüm'});
  await expect(page.getByLabel('Çizim alanı')).toBeAttached();
  await expect(switcher.getByRole('button',{name:'Sayfalar'})).toHaveAttribute('aria-pressed','true');
  await expect(page.getByRole('textbox',{name:'Not içeriği'})).toHaveCount(0);
  await switcher.getByRole('button',{name:'Metni düzenle'}).click();
  const editor=page.getByRole('textbox',{name:'Not içeriği'});
  await expect(editor).toHaveAttribute('contenteditable','true');await expect(editor).toContainText('Kalem notunun metni');
  await expect(page.getByLabel('Çizim alanı')).toHaveCount(0);
  await expect(switcher).toContainText('El yazısı ve çizimler korunur');
  await editor.click();await page.keyboard.press('End');await page.keyboard.type(' ve ek');
  await page.getByRole('button',{name:/Kaydet/}).click();
  await expect.poll(()=>requests.filter(r=>r.method==='PUT').length).toBe(1);
  const put=requests.find(r=>r.method==='PUT')!;
  expect(put.path).toBe('/api/notes/pen-text/editor');expect(put.data.model.text).toBe('Kalem notunun metni ve ek');
  expect(requests.some(r=>r.method!=='GET'&&r.path.endsWith('/ink'))).toBe(false);
  await switcher.getByRole('button',{name:'Sayfalar'}).click();
  await expect(page.getByLabel('Çizim alanı')).toBeAttached();
});
