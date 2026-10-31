import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
const TOKEN='synthetic_account_ui_session_123456';
async function accountFixture(page:Page,connected=false){
  const state={connected,accountLabel:connected?'Test hesabı':null as string|null,loginPending:false,loginError:null as string|null,
    sync:{state:'idle',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},
    capabilities:{textEditing:true,cloudDelete:false,cloudTrash:true,cloudFavorites:true,cloudFolders:true,attachments:false}};
  const calls:string[]=[];let failLogin=false;
  await page.route('**/api/**',async route=>{
    expect(route.request().headers().authorization).toBe(`Bearer ${TOKEN}`);
    const path=new URL(route.request().url()).pathname;
    const send=(data:unknown,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(data)});
    if(path==='/api/preferences')return send(route.request().method()==='PUT'?route.request().postDataJSON():{version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(path==='/api/status')return send(state);
    if(path==='/api/notes')return send({notes:[]});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/auth/login'){
      calls.push('login');
      if(failLogin)return send({error:{code:'offline_mode',message:'Bu oturum çevrimdışı başlatıldı. Normal başlatıcıyla yeniden aç.'}},409);
      state.loginPending=true;state.loginError=null;return send({started:true},202);
    }
    if(path==='/api/auth/disconnect'){
      calls.push('disconnect');state.connected=false;state.accountLabel=null;return send({connected:false});
    }
    return send({error:{code:'unexpected',message:'Unexpected synthetic request'}},404);
  });
  await page.goto('/#token='+TOKEN);
  const settings=page.getByRole('button',{name:'Ayarlar',exact:true});
  if((page.viewportSize()?.width??1440)<=800)await page.getByRole('button',{name:'Menüyü aç',exact:true}).click();
  await settings.click();
  await expect(page.locator('.account-panel')).toBeVisible();
  return {state,calls,setFailLogin:(value:boolean)=>{failLogin=value;}};
}
test('account login waits for real status, avoids duplicate start and reflects confirmed connection',async({page})=>{
  const errors:string[]=[];page.on('pageerror',error=>errors.push(error.name));
  const {state,calls}=await accountFixture(page);
  await expect(page).toHaveTitle(/Not/);
  await expect(page).toHaveURL(new URL('/',page.url()).href);
  await expect(page.locator('vite-error-overlay')).toHaveCount(0);
  await expect(page.getByRole('heading',{name:'Samsung hesabınızla bağlanın'})).toBeVisible();
  expect(await page.locator('input[type=password]').count()).toBe(0);
  await page.getByRole('button',{name:'Samsung hesabıyla bağlan',exact:true}).click();
  await expect(page.getByRole('button',{name:'Tarayıcıda giriş bekleniyor'})).toBeDisabled();
  await expect(page.getByText('Tarayıcıda açılan Samsung girişini tamamlayın.')).toBeVisible();
  expect(calls).toEqual(['login']);
  state.loginPending=false;state.connected=true;state.accountLabel='Test hesabı';
  await expect(page.getByRole('heading',{name:'Samsung hesabı bağlı'})).toBeVisible({timeout:7000});
  await expect(page.getByText('Test hesabı',{exact:true})).toBeVisible();
  expect(calls).toEqual(['login']);
  await page.screenshot({path:`${process.env.TMPDIR||'/tmp'}/samsung-account-connected-synthetic.png`});
  expect(errors).toEqual([]);
});
test('failed browser login has inline error and retry, not a false connected state',async({page})=>{
  const {state,calls}=await accountFixture(page);
  await page.getByRole('button',{name:'Samsung hesabıyla bağlan',exact:true}).click();
  await expect(page.getByRole('button',{name:'Tarayıcıda giriş bekleniyor'})).toBeDisabled();
  state.loginPending=false;state.loginError='Giriş tamamlanamadı. Yeniden giriş yapabilirsiniz.';
  await expect(page.locator('.account-panel').getByRole('alert')).toContainText('Giriş tamamlanamadı',{timeout:7000});
  await expect(page.getByRole('button',{name:'Yeniden giriş yap',exact:true})).toBeEnabled();
  await expect(page.getByRole('button',{name:'Bağlantıyı kes',exact:true})).toHaveCount(0);
  await page.getByRole('button',{name:'Yeniden giriş yap',exact:true}).click();
  expect(calls).toEqual(['login','login']);
});
test('disconnect is explicit and preserves local/offline explanation',async({page})=>{
  await page.setViewportSize({width:390,height:844});
  const {calls}=await accountFixture(page,true);
  await expect(page.locator('.account-description')).toHaveCount(0);
  const disconnect=page.getByRole('button',{name:'Bağlantıyı kes',exact:true});
  await expect(disconnect).toHaveAttribute('title',/notları silmez/);
  await disconnect.click();
  await expect(page.getByRole('heading',{name:'Samsung hesabınızla bağlanın'})).toBeVisible();
  await expect(page.getByRole('button',{name:'Samsung hesabıyla bağlan',exact:true})).toBeEnabled();
  expect(calls).toEqual(['disconnect']);
  await page.locator('.settings-page-scroll').evaluate(element=>{element.scrollTop=0;});
  await page.screenshot({path:`${process.env.TMPDIR||'/tmp'}/samsung-account-offline-mobile-synthetic.png`});
});
test('account action failure does not appear connected or repeat automatically',async({page})=>{
  const {calls,setFailLogin}=await accountFixture(page);
  setFailLogin(true);
  await page.getByRole('button',{name:'Samsung hesabıyla bağlan',exact:true}).click();
  await expect(page.getByText(/Bu oturum çevrimdışı başlatıldı|Giriş başlatılamadı/).first()).toBeVisible();
  await expect(page.getByRole('heading',{name:'Samsung hesabınızla bağlanın'})).toBeVisible();
  expect(calls).toEqual(['login']);
});
