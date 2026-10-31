import {test,expect} from '@playwright/test';
import {DEFAULT_SHORTCUTS} from '../src/useShortcuts';

test('slow migration shows only progress and then opens the library',async({page})=>{
  let complete=false, progress=7;
  const requests:string[]=[];
  await page.route('**/api/**',async route=>{
    const path=new URL(route.request().url()).pathname;
    requests.push(path);
    const send=(body:unknown,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(body)});
    if(path==='/api/status')return complete?send({connected:false,accountLabel:null,loginPending:false,sync:{state:'offline',lastSyncedAt:null,pending:0,conflicts:0,progress:{processed:0,total:0},error:null},capabilities:{textEditing:true,cloudDelete:false,cloudTrash:false,cloudFavorites:false,cloudFolders:false,attachments:false}}):send({migration:{state:'running',completed:progress,total:50}});
    if(!complete)return send({error:{code:'migration_in_progress'}},503);
    if(path==='/api/notes')return send({notes:[]});
    if(path==='/api/folders')return send({folders:[],trash:[],initialized:true});
    if(path==='/api/preferences')return send({version:1,galleryMode:'grid',sortAscending:true,toolsOpen:true,defaultZoom:100});
    if(path==='/api/shortcuts')return send({schemaVersion:2,bindings:DEFAULT_SHORTCUTS});
    return send({});
  });
  await page.goto('/#token=synthetic_local_session_1234567890');
  await expect(page.getByText('Notlar yeni depolama biçimine taşınıyor')).toBeVisible();
  await expect(page.getByText('7/50')).toBeVisible();
  await expect(page.getByRole('progressbar')).toHaveAttribute('value','7');
  await expect(page.locator('.alert')).toHaveCount(0);
  await expect(page.getByRole('heading',{name:'Tüm notlar',exact:true})).toHaveCount(0);
  expect(requests.every(path=>path==='/api/status')).toBe(true);
  progress=28;
  await expect(page.getByText('28/50')).toBeVisible();
  complete=true;
  await expect(page.getByRole('heading',{name:'Tüm notlar',exact:true})).toBeVisible();
  await expect(page.getByRole('progressbar')).toHaveCount(0);
});
