// Engine reasons are pinned by the frozen conformance fixtures, so the few that
// read like internal states are shown here in plain words. Unknown text passes through.
const PLAIN:Record<string,string>={
  'Bu notun genişletilmiş kalem biçimi henüz desteklenmiyor.':'Bu notun kalem biçimi şimdilik desteklenmiyor.',
  'Bu notun kalem biçimi güvenli biçimde açılamıyor.':'Bu notun çizimleri şimdilik açılamıyor.',
  'Bu notun çizim verisi gösterilebilir; bu biçimde kalem ekleme henüz desteklenmiyor.':'Bu notta şimdilik yeni çizim eklenemiyor.',
  'Bu sayfadaki çizgilerin kalem türü henüz kopyalanamıyor; yeni çizgi eklenemiyor.':'Bu sayfaya şimdilik yeni çizgi eklenemiyor.',
  'Bu sayfanın katman yapısında ilk çizgi henüz desteklenmiyor.':'Bu sayfaya şimdilik ilk çizgi eklenemiyor.',
  'Bu sayfanın çizgi geometrisi güvenle okunamadı.':'Bu sayfanın çizgileri okunamadı.',
  'Bu not için desteklenen bir kalem şablonu bulunamadı.':'Bu notta şimdilik çizim eklenemiyor.',
};
export function plainReason(text:string):string;
export function plainReason(text:string|null|undefined):string|null|undefined;
export function plainReason(text:string|null|undefined){return text?PLAIN[text]??text:text;}
