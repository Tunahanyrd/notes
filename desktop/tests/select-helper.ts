import type {Page} from '@playwright/test';
export async function chooseOption(page:Page,label:string,value:string){
  await page.getByRole('combobox',{name:label,exact:true}).click();
  await page.getByRole('listbox',{name:label,exact:true}).locator(`[role="option"][data-value="${value}"]`).click();
}
