const KEY = 'linux-notes.local-token.v1';
export function captureToken(): string {
  const fragment = new URLSearchParams(location.hash.slice(1));
  const incoming = fragment.get('token');
  if (incoming) {
    history.replaceState(null, '', location.pathname + location.search);
    if (!/^[A-Za-z0-9_-]{20,256}$/.test(incoming)) throw new Error('Geçersiz uygulama oturumu. Uygulamayı yeniden açın.');
    sessionStorage.setItem(KEY, incoming);
  }
  return sessionStorage.getItem(KEY) || '';
}
export class ApiError extends Error {constructor(message:string, public status:number, public code:string) {super(message);}}
let token = '';
export function initializeApi() { token = captureToken(); return !!token; }
export async function request(path:string, options:RequestInit = {}) {
  if (!path.startsWith('/api/') || path.includes('://')) throw new Error('Geçersiz uygulama isteği.');
  if (!token) throw new ApiError('Uygulama oturumu yok. Notlar uygulamasını yeniden açın.', 401, 'session_missing');
  let response:Response;
  try { response = await fetch(path, {...options, credentials:'omit', redirect:'error', headers:{'Content-Type':'application/json', ...options.headers, Authorization:`Bearer ${token}`}}); }
  catch(e) { if (e instanceof DOMException && e.name === 'AbortError') throw e; throw new ApiError('Yerel hizmete ulaşılamıyor. Açık metninizi koruyun ve yeniden deneyin.', 0, 'unreachable'); }
  if (!response.ok) {
    const value = await response.json().catch(()=>null);
    throw new ApiError(value?.error?.message || 'İşlem tamamlanamadı. Lütfen yeniden deneyin.', response.status, value?.error?.code || 'request_failed');
  }
  return response;
}
export async function api<T>(path:string, method='GET', body?:unknown, signal?:AbortSignal):Promise<T> {
  return (await request(path, {method, signal, ...(body === undefined ? {} : {body:JSON.stringify(body)})})).json();
}
export const notePath = (id:string) => `/api/notes/${encodeURIComponent(id)}`;
