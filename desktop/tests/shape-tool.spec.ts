import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
// Shapes tool (Şekil): drag on a page queues one page-ops 'shape' job; saved
// shapes are drawn from GET /shapes; disabled pages explain why. Synthetic data only.
const TOKEN='synthetic_local_session_1234567890';
const PNG=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lfoAAAAASUVORK5CYII=','base64');
async function fixture(page:Page,options:{shapeReason?:string;saved?:boolean;twoPages?:boolean}={}){
  const requests:{path:string;method:string;data:any}[]=[];
  const note:any={folderId:null,metadataSync:{state:'synced',error:null},createdAt:1777000000000,modifiedAt:1777000000000,revision:1,syncState:'synced',
    editable:false,titleEditable:true,readOnlyReason:'Çizim notu.',favorite:false,trashed:false,contentLoaded:true,source:'cloud',error:null,body:'',preview:'',
    id:'shape-note',title:'Şekil denemesi'};
  const saved=options.saved?[{id:'shape-1',kind:'rectangle',geometry:[100,100,400,300],colorARGB:0xffff343b,width:6,fillARGB:null}]:[];
  const shapePages:any[]=[{id:'page-0',index:0,width:1000,height:1000,addShape:!options.shapeReason,reason:options.shapeReason??null,shapes:options.twoPages?[]:saved}];
  if(options.twoPages)shapePages.push({id:'page-1',index:1,width:1000,height:1414,addShape:true,reason:null,shapes:saved});
  await page.route('**/local-fonts/*',route=>route.fulfill({status:404,body:''}));
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),path=url.pathname,method=route.request().method(),data=method==='GET'?null:route.request().postDataJSON();
    const send=(value:any,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
    if(path.endsWith('/ink-draft'))return send({revision:null,strokes:[]});
    requests.push({path,method,data});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send({connected:true,accountLabel:'Sentetik hesap',loginPending:false,sync:{state:'idle',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,cloudDelete:false,richTextEditing:false}});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/notes')return send({notes:[{...note,body:undefined}]});
    if(path.endsWith('/thumbnail')||path.endsWith('/preview'))return route.fulfill({contentType:'image/png',body:PNG});
    if(path.endsWith('/editor'))return send({schemaVersion:1,revision:note.revision,model:null,reason:'Bilinmeyen biçimlendirme sözdizimi.',capabilities:{titleEdit:true,textEdit:false,richStyle:false,inkEdit:false}});
    if(path.endsWith('/pages'))return send({providerAvailable:true,pages:[{index:0,width:1000,height:1000,available:true,complete:true},...(options.twoPages?[{index:1,width:null,height:null,available:false}]:[])]});
    // The pen projection is disabled for notes with shapes, exactly as the backend does.
    if(path.endsWith('/ink')&&method==='GET')return send({schemaVersion:1,revision:note.revision,pages:[],capabilities:{appendInk:false,eraseInk:false,firstStroke:false},reason:'Bu notta şekil veya metin kutusu gibi nesneler var; kalem henüz desteklenmiyor.',rendering:{nativeEquivalent:false,mode:'geometry-only'},pendingStrokes:[]});
    if(path.endsWith('/shapes'))return send({schemaVersion:1,revision:note.revision,kinds:['rectangle','ellipse','triangle','rightTriangle','diamond','line','arrow'],
      pages:shapePages.map(p=>({...p,addShape:p.addShape&&note.syncState==='synced'})),reason:options.shapeReason??null,rendering:{nativeEquivalent:false,mode:'geometry-only'}});
    if(path.endsWith('/page-ops')&&method==='POST'){note.revision++;note.syncState='pending';return send({note});}
    if(method==='GET')return send({note});return send({error:{code:'unexpected_mutation',message:'Beklenmeyen değişiklik'}},422);
  });
  await page.goto('/#token='+TOKEN);await page.getByRole('button',{name:/Şekil denemesi/}).click();
  await expect(page.locator('.page-sheet').first()).toBeVisible();
  return {requests,note};
}
const rail=(page:Page)=>page.locator('.tool-rail');
test('dragging with the ellipse queues one page-ops shape job and shows it while it syncs',async({page})=>{
  const {requests}=await fixture(page);
  const button=rail(page).getByRole('button',{name:'Şekil',exact:true});
  await expect(button).toBeEnabled();await expect(button).toHaveAttribute('title','Şekil');
  await button.click();
  const panel=page.getByRole('complementary',{name:'Şekil araçları'});await expect(panel).toBeVisible();
  await panel.getByRole('radio',{name:'Elips'}).click();
  await panel.getByRole('button',{name:'Çizgi rengi 5',exact:true}).click();
  const area=page.getByLabel('1. sayfa şekil alanı');await expect(area).toHaveClass(/drawing/);
  const box=(await area.boundingBox())!;const at=(x:number,y:number)=>[box.x+x*box.width/1000,box.y+y*box.height/1000] as const;
  await page.mouse.move(...at(200,200));await page.mouse.down();await page.mouse.move(...at(500,400),{steps:6});
  await expect(page.locator('.shape-preview')).toBeVisible();
  await page.mouse.up();
  await expect.poll(()=>requests.filter(r=>r.path.endsWith('/page-ops')).length).toBe(1);
  const post=requests.find(r=>r.path.endsWith('/page-ops'))!;
  expect(post.data.op).toBe('shape');expect(post.data.pageId).toBe('page-0');expect(post.data.expectedRevision).toBe(1);
  expect(post.data.shape.kind).toBe('ellipse');expect(post.data.shape.colorARGB).toBe(0xff229bff);expect(post.data.shape.width).toBe(4);
  expect(post.data.shape.fillARGB).toBeUndefined();
  const [x1,y1,x2,y2]=post.data.shape.geometry;
  for(const [value,expected] of [[x1,200],[y1,200],[x2,500],[y2,400]])expect(Math.abs(value-expected)).toBeLessThan(6);
  await expect(page.locator('.shape-pending')).toBeAttached();
  // The next shape waits for the Cloud write.
  await expect(page.locator('.shape-notice')).toContainText('eşitleniyor');
  await expect(page.getByLabel('1. sayfa şekil alanı')).toHaveCount(0);
});
// Saved shapes are part of the page image (canvas cache or local renderer):
// over an image only the shape still being synced is drawn.
test('saved shapes come from the page image and a page that cannot take shapes explains why',async({page})=>{
  const {requests}=await fixture(page,{saved:true,shapeReason:'Sayfa katmanı kilitli veya gizli; şekil eklenemiyor.'});
  await expect(page.locator('.page-sheet[data-page-index="0"] img.page-image')).toBeVisible();
  await expect(page.locator('.shape-overlay rect')).toHaveCount(0);
  const button=rail(page).getByRole('button',{name:'Şekil',exact:true});
  await expect(button).toBeDisabled();
  await expect(button).toHaveAttribute('title','Şekil: Sayfa katmanı kilitli veya gizli; şekil eklenemiyor.');
  expect(requests.filter(r=>r.method!=='GET')).toHaveLength(0);
});
test('a line ignores the fill option and a tiny drag sends nothing',async({page})=>{
  const {requests}=await fixture(page);
  await rail(page).getByRole('button',{name:'Şekil',exact:true}).click();
  const panel=page.getByRole('complementary',{name:'Şekil araçları'});
  await panel.getByRole('checkbox',{name:'Dolgu'}).check();
  await panel.getByRole('radio',{name:'Ok'}).click();
  await expect(panel.getByRole('checkbox',{name:'Dolgu'})).toBeDisabled();
  const area=page.getByLabel('1. sayfa şekil alanı');const box=(await area.boundingBox())!;
  await page.mouse.move(box.x+100,box.y+100);await page.mouse.down();await page.mouse.move(box.x+101,box.y+101);await page.mouse.up();
  await expect(page.locator('.shape-notice')).toContainText('sürükleyin');
  expect(requests.filter(r=>r.path.endsWith('/page-ops'))).toHaveLength(0);
  await page.mouse.move(box.x+100,box.y+100);await page.mouse.down();await page.mouse.move(box.x+300,box.y+150,{steps:4});await page.mouse.up();
  await expect.poll(()=>requests.filter(r=>r.path.endsWith('/page-ops')).length).toBe(1);
  const shape=requests.find(r=>r.path.endsWith('/page-ops'))!.data.shape;
  expect(shape.kind).toBe('arrow');expect(shape.fillARGB).toBeUndefined();
});
test('a page without a preview image still shows its shapes and takes new ones',async({page})=>{
  const {requests}=await fixture(page,{saved:true,twoPages:true});
  await expect(page.locator('.page-sheet[data-page-index="1"] .page-placeholder')).toHaveText('Bu sayfanın görüntüsü bu cihazda oluşturulamıyor.');
  await expect(page.getByText(/görüntüsü yok|yaklaşık geometri/)).toHaveCount(0);
  await expect(page.locator('.page-sheet[data-page-index="1"] .shape-overlay rect')).toHaveCount(1);
  await rail(page).getByRole('button',{name:'Şekil',exact:true}).click();
  const area=page.getByLabel('2. sayfa şekil alanı');await area.scrollIntoViewIfNeeded();const box=(await area.boundingBox())!;
  await page.mouse.move(box.x+50,box.y+50);await page.mouse.down();await page.mouse.move(box.x+250,box.y+200,{steps:4});await page.mouse.up();
  await expect.poll(()=>requests.filter(r=>r.path.endsWith('/page-ops')).length).toBe(1);
  expect(requests.find(r=>r.path.endsWith('/page-ops'))!.data.pageId).toBe('page-1');
});
