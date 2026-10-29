import {test,expect,type Page} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';
const TOKEN='settings_synthetic_session_123456789';
async function setup(page:Page,options:{connected?:boolean;badStorage?:boolean}={}) {
  const writes:string[]=[];
  let preferences:{version:number;theme?:string;darkPages?:boolean;galleryMode:string;sortAscending:boolean;toolsOpen:boolean;defaultZoom:number}={version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100};
  const errors:string[]=[];
  page.on('pageerror',e=>errors.push(e.message));
  if(options.badStorage)await page.addInitScript(()=>localStorage.setItem('samsung-notes.preferences.v1','{broken'));
  await page.route('**/local-fonts/*',route=>route.fulfill({status:404,body:''}));
  await page.route('**/api/**',async route=>{
    const url=new URL(route.request().url()),method=route.request().method();
    expect(route.request().headers()['authorization']).toBe(`Bearer ${TOKEN}`);
    if(method!=='GET')writes.push(`${method} ${url.pathname}`);
    const send=(value:unknown)=>route.fulfill({contentType:'application/json',body:JSON.stringify(value)});
    if(url.pathname==='/api/shortcuts'&&method==='GET')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    if(url.pathname==='/api/preferences'){if(method==='PUT')preferences=route.request().postDataJSON();return send(preferences);}
    if(url.pathname==='/api/status')return send({connected:options.connected!==false,accountLabel:'Sentetik test hesabı',loginPending:false,sync:{state:'idle',lastSyncedAt:1777000000000,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudTrash:true,cloudFavorites:true,cloudFolders:true,attachments:false,cloudDelete:false}});
    if(url.pathname==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(url.pathname==='/api/notes')return send({notes:[]});
    if(url.pathname==='/api/sync')return send({accepted:true});
    return route.fulfill({status:404,contentType:'application/json',body:JSON.stringify({error:{message:'Beklenmeyen test isteği'}})});
  });
  await page.goto('/#token='+TOKEN);
  if((page.viewportSize()?.width??1440)<800)await page.getByRole('button',{name:'Menüyü aç',exact:true}).click();
  await expect(page.getByRole('button',{name:'Ayarlar',exact:true})).toBeVisible();
  await page.getByRole('button',{name:'Ayarlar',exact:true}).click();
  await expect(page.getByRole('dialog',{name:'Ayarlar',exact:true})).toBeVisible();
  return {writes,errors,getPreferences:()=>preferences};
}
test('settings persist only working device preferences and call explicit sync',async({page})=>{
  const {writes,errors,getPreferences}=await setup(page);
  await page.getByRole('button',{name:'Liste',exact:true}).click();
  await page.getByLabel('Not sıralaması',{exact:true}).selectOption('newest');
  await page.getByRole('switch',{name:'Metin araçlarını göster'}).uncheck();
  await page.getByLabel('Varsayılan yakınlaştırma',{exact:true}).selectOption('125');
  await expect.poll(getPreferences).toEqual({version:1,theme:'system',darkPages:false,galleryMode:'list',sortAscending:false,toolsOpen:false,defaultZoom:125});
  await expect(page.getByLabel('Varsayılan yakınlaştırma',{exact:true})).toHaveValue('125');
  expect(writes.every(path=>path==='PUT /api/preferences')).toBe(true);
  await page.getByRole('button',{name:'Şimdi eşitle',exact:true}).click();
  await expect.poll(()=>writes.filter(path=>path!=='PUT /api/preferences')).toEqual(['POST /api/sync']);
  await page.getByRole('button',{name:'Ayarları kapat'}).click();
  await expect(page.locator('.note-rows')).toHaveClass(/gallery-list/);
  await page.reload();
  await page.getByRole('button',{name:'Ayarlar',exact:true}).click();
  await expect(page.getByRole('button',{name:'Liste',exact:true})).toHaveAttribute('aria-pressed','true');
  await expect(page.getByLabel('Not sıralaması',{exact:true})).toHaveValue('newest');
  await expect(page.getByRole('switch',{name:'Metin araçlarını göster'})).not.toBeChecked();
  await expect(page.getByLabel('Varsayılan yakınlaştırma',{exact:true})).toHaveValue('125');
  expect(errors).toEqual([]);
  await page.getByRole('dialog',{name:'Ayarlar',exact:true}).evaluate(el=>Promise.all(el.getAnimations().map(animation=>animation.finished)));
  await page.screenshot({path:'/tmp/notes-settings-desktop.png'});
});
test('settings narrow layout stays bounded, keyboard close restores focus',async({page})=>{
  await page.setViewportSize({width:390,height:844});
  const {writes,errors}=await setup(page,{connected:false,badStorage:true});
  const dialog=page.getByRole('dialog',{name:'Ayarlar',exact:true});
  await expect(page.getByRole('button',{name:'Şimdi eşitle',exact:true})).toBeDisabled();
  await expect(page.getByRole('button',{name:'Izgara',exact:true})).toHaveAttribute('aria-pressed','true');
  // Measure the settled layout: dialog-enter starts at scale(1.03), so a box read
  // mid-animation is 3 % larger (seen on slower macOS runners).
  await dialog.evaluate(el=>Promise.all(el.getAnimations().map(animation=>animation.finished)));
  const bounds=await dialog.boundingBox();expect(bounds).not.toBeNull();expect(bounds!.x).toBeGreaterThanOrEqual(0);expect(bounds!.x+bounds!.width).toBeLessThanOrEqual(390);expect(bounds!.height).toBeLessThanOrEqual(844);
  expect(await dialog.evaluate(el=>el.scrollWidth<=el.clientWidth)).toBe(true);
  await page.getByLabel('Varsayılan yakınlaştırma',{exact:true}).scrollIntoViewIfNeeded();
  await page.screenshot({path:'/tmp/notes-settings-mobile.png'});
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(page.getByRole('button',{name:'Ayarlar',exact:true})).toBeFocused();
  expect(writes).toEqual([]);expect(errors).toEqual([]);
});

test('theme selection follows system and persists',async({page})=>{
  await page.emulateMedia({colorScheme:'light'});
  const {getPreferences}=await setup(page);
  const theme=page.getByLabel('Tema',{exact:true});
  const color=()=>page.locator('.sidebar').evaluate(el=>getComputedStyle(el).backgroundColor);
  const light=await color();
  await theme.selectOption('dark');
  await expect.poll(color).not.toBe(light);
  await expect.poll(()=>getPreferences().theme).toBe('dark');
  await theme.selectOption('light');
  await expect.poll(color).toBe(light);
  await theme.selectOption('system');
  await page.emulateMedia({colorScheme:'dark'});
  await expect.poll(color).not.toBe(light);
  await page.emulateMedia({colorScheme:'light'});
  await expect.poll(color).toBe(light);
  await page.getByRole('switch',{name:'Not sayfalarını koyu göster'}).check();
  await expect.poll(()=>getPreferences().darkPages).toBe(true);
});
