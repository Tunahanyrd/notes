import type {JSONContent} from '@tiptap/core';

export const spanKinds=['bold','italic','underline','strikethrough','fontSize','foregroundColor','backgroundColor','hyperText'] as const;
export type SpanKind=typeof spanKinds[number];
// Samsung hyperText span (s1.o): WDocHyperTextSpan type/dateTimeType and an
// optional data field (null = field absent on the wire). Preserved as is.
export interface HyperTextValue {type:number;dateTimeType:number;data:string|null}
export type SpanValue=boolean|number|string|HyperTextValue;
export interface EditorSpan {start:number;end:number;kind:SpanKind;value:SpanValue;interval:0|1|2|3}
// SpenBulletParagraph types accepted by the SDK setter (0,2,4,6..12). The
// toolbar creates only none/disc/number/check; the others are preserved and
// rendered when a note from Android/Windows already contains them.
export const bulletTypes={none:0,check:2,number:4,alphabet:6,roman:7,disc:8,whiteCircle:9,upperAlphabet:10,blackSquare:11,whiteSquare:12} as const;
export type BulletKind=keyof typeof bulletTypes;
export const numberedBullets:readonly BulletKind[]=['number','alphabet','roman','upperAlphabet'];
export interface EditorParagraph {index:number;alignment?:'left'|'right'|'center'|'justify';parsingState?:boolean;bullet?:{kind:BulletKind;number?:number;checked?:boolean;typeList?:number[]};indentLevel?:{level:number;direction:'none'|'ltr'|'rtl'}}
export interface EditorModel {schemaVersion:1;text:string;spans:EditorSpan[];paragraphs:EditorParagraph[]}
const invalid=()=>new Error('Bu notun biçimlendirmesi güvenle düzenlenemiyor. Özgün içerik korunuyor.');
const same=(a:unknown,b:unknown)=>JSON.stringify(a)===JSON.stringify(b);
const boundary=(text:string,at:number)=>Number.isInteger(at)&&at>=0&&at<=text.length&&!(at>0&&at<text.length&&/[\uD800-\uDBFF]/.test(text[at-1])&&/[\uDC00-\uDFFF]/.test(text[at]));
function valueValid(span:EditorSpan){
  if(['bold','italic','underline','strikethrough'].includes(span.kind))return typeof span.value==='boolean';
  if(span.kind==='fontSize')return typeof span.value==='number'&&Number.isFinite(span.value)&&span.value>0&&span.value<=1000;
  if(span.kind==='hyperText'){const v=span.value as HyperTextValue;return !!v&&typeof v==='object'&&Object.keys(v).sort().join()==='data,dateTimeType,type'&&Number.isInteger(v.type)&&v.type>=0&&v.type<10&&Number.isInteger(v.dateTimeType)&&v.dateTimeType>=0&&v.dateTimeType<22&&(v.data===null||(typeof v.data==='string'&&v.data.length<=4096&&!/[,'\x00-\x1f]|<\?|\?>/.test(v.data)));}
  return typeof span.value==='string'&&/^#[\da-f]{8}$/i.test(span.value);
}
export function assertEditorModel(model:EditorModel){
  if(model.schemaVersion!==1||typeof model.text!=='string'||model.text.length>262144||!Array.isArray(model.spans)||!Array.isArray(model.paragraphs)||model.spans.length+model.paragraphs.length>32768)throw invalid();
  for(const span of model.spans){
    if(!spanKinds.includes(span.kind)||!boundary(model.text,span.start)||!boundary(model.text,span.end)||span.start>span.end||![0,1,2,3].includes(span.interval)||!valueValid(span))throw invalid();
    // A zero-length SDK annotation is a typing-style anchor, not an empty mark.
    // Do not silently discard it in ProseMirror's non-empty text mark model.
    if(span.start===span.end)throw new Error('Bu notta konuma bağlı yazım biçimi var. Bu biçimi kaybetmeden düzenleme henüz desteklenmiyor.');
  }
  const seen=new Set<number>(),count=model.text.split(/[\r\n]/).length;
  if(model.paragraphs.some(p=>p.index===0)&&(!model.text.length||/^[\r\n]/.test(model.text)))throw invalid();
  for(const paragraph of model.paragraphs){if(!Number.isInteger(paragraph.index)||paragraph.index<0||paragraph.index>=count||seen.has(paragraph.index)||Object.keys(paragraph).some(k=>!['index','alignment','parsingState','bullet','indentLevel'].includes(k))||(paragraph.alignment!==undefined&&!['left','right','center','justify'].includes(paragraph.alignment))||(paragraph.parsingState!==undefined&&typeof paragraph.parsingState!=='boolean'))throw invalid();if(paragraph.indentLevel&&(!Number.isInteger(paragraph.indentLevel.level)||paragraph.indentLevel.level<0||paragraph.indentLevel.level>32||!['none','ltr','rtl'].includes(paragraph.indentLevel.direction)))throw invalid();if(paragraph.bullet&&(!Object.hasOwn(bulletTypes,paragraph.bullet.kind)||(paragraph.bullet.kind==='check'?typeof paragraph.bullet.checked!=='boolean'||paragraph.bullet.number!==undefined:paragraph.bullet.checked!==undefined||!Number.isInteger(paragraph.bullet.number??0)||(paragraph.bullet.number??0)<0||(paragraph.bullet.number??0)>2147483647)||(paragraph.bullet.typeList&&(!Array.isArray(paragraph.bullet.typeList)||paragraph.bullet.typeList.length>32||paragraph.bullet.typeList.some(v=>!(Object.values(bulletTypes) as number[]).includes(v))))))throw invalid();seen.add(paragraph.index);}
  for(let i=0;i<model.spans.length;i++)for(let j=i+1;j<model.spans.length;j++){const a=model.spans[i],b=model.spans[j];if(a.kind===b.kind&&Math.max(a.start,b.start)<Math.min(a.end,b.end))throw invalid();}
}
export const markName=(kind:SpanKind)=>`samsung_${kind}`;
export function modelToContent(model:EditorModel):JSONContent{
  assertEditorModel(model);
  const marksAt=(start:number,end:number)=>model.spans.flatMap((span,index)=>span.start<=start&&span.end>=end?[{type:markName(span.kind),attrs:{value:span.value,interval:span.interval,sourceId:`source-${index}`,sourceOrder:index}}]:[]);
  const pieces=model.text.split(/([\r\n])/);let offset=0;
  const content:JSONContent[]=[];
  for(let i=0;i<pieces.length;i+=2){
    const text=pieces[i],start=offset,end=start+text.length,separator=pieces[i+1]||'';
    const boundaries=[...new Set([start,end,...model.spans.flatMap(s=>[s.start,s.end]).filter(p=>p>start&&p<end)])].sort((a,b)=>a-b);
    const children:JSONContent[]=[];
    for(let j=0;j<boundaries.length-1;j++){const a=boundaries[j],b=boundaries[j+1];if(a<b)children.push({type:'text',text:model.text.slice(a,b),marks:marksAt(a,b)});}
    const paragraph=model.paragraphs.find(p=>p.index===content.length);
    content.push({type:'paragraph',attrs:{nativeAlignment:paragraph?.alignment??null,nativeParsingState:paragraph?.parsingState??null,nativeBullet:paragraph?.bullet??null,nativeIndent:paragraph?.indentLevel??null,separatorAfter:separator,separatorMarks:separator?marksAt(end,end+1):[]},content:children});
    offset=end+separator.length;
  }
  return {type:'doc',content};
}
export function contentToModel(doc:JSONContent):EditorModel{
  if(doc.type!=='doc'||!doc.content?.length)throw invalid();
  let text='';const paragraphs:EditorParagraph[]=[],segments:{span:EditorSpan;id:string;order:number}[]=[];
  const append=(value:string,marks:JSONContent['marks'])=>{
    const start=text.length;text+=value;
    for(const mark of marks||[]){
      const kind=spanKinds.find(k=>markName(k)===mark.type);if(!kind)throw invalid();
      const attrs=mark.attrs||{},interval=attrs.interval??3;
      segments.push({span:{start,end:text.length,kind,value:attrs.value,interval},id:String(attrs.sourceId??`new-${kind}`),order:Number(attrs.sourceOrder??100000)});
    }
  };
  doc.content.forEach((paragraph,index)=>{
    if(paragraph.type!=='paragraph')throw invalid();
    const attrs=paragraph.attrs||{},start=text.length;
    for(const child of paragraph.content||[]){if(child.type!=='text'||typeof child.text!=='string')throw invalid();append(child.text,child.marks);}
    if((attrs.nativeAlignment!==null&&attrs.nativeAlignment!==undefined)||(attrs.nativeParsingState!==null&&attrs.nativeParsingState!==undefined)||attrs.nativeBullet!=null||attrs.nativeIndent!=null){
      // Native paragraphs after the first include the preceding newline. An empty
      // first paragraph has no serializable range; retain its transient typing
      // attributes in ProseMirror, but do not invent an invalid saved annotation.
      if(index>0||text.length>start)paragraphs.push({index,...(attrs.nativeAlignment!=null?{alignment:attrs.nativeAlignment}:{}),...(attrs.nativeParsingState!=null?{parsingState:attrs.nativeParsingState}:{}),...(attrs.nativeBullet!=null?{bullet:attrs.nativeBullet}:{}),...(attrs.nativeIndent!=null?{indentLevel:attrs.nativeIndent}:{})});
    }
    if(index<doc.content!.length-1){const separator=['\n','\r'].includes(attrs.separatorAfter)?attrs.separatorAfter:'\n';append(separator,attrs.separatorMarks||[]);}
  });
  const joined:typeof segments=[];
  for(const segment of segments){
    const previous=joined.filter(s=>s.id===segment.id&&s.span.kind===segment.span.kind).at(-1);
    if(previous&&previous.span.end===segment.span.start&&previous.span.interval===segment.span.interval&&same(previous.span.value,segment.span.value))previous.span.end=segment.span.end;
    else joined.push({...segment,span:{...segment.span}});
  }
  joined.sort((a,b)=>a.order-b.order||a.span.start-b.span.start);
  const model:EditorModel={schemaVersion:1,text,spans:joined.map(s=>s.span),paragraphs};assertEditorModel(model);return model;
}
const roman=(n:number)=>{let out='';for(const [v,r] of [[1000,'m'],[900,'cm'],[500,'d'],[400,'cd'],[100,'c'],[90,'xc'],[50,'l'],[40,'xl'],[10,'x'],[9,'ix'],[5,'v'],[4,'iv'],[1,'i']] as const)while(n>=v){out+=r;n-=v;}return out;};
const letters=(n:number)=>{let out='';while(n>0){n--;out=String.fromCharCode(97+n%26)+out;n=Math.floor(n/26);}return out;};
// Display-only marker text. Native glyphs, punctuation and spacing are not
// established from Samsung source; this is an approximation of the list kind.
export function bulletMarker(bullet:{kind:BulletKind;number?:number;checked?:boolean}|null|undefined):string{
  if(!bullet)return '';const n=Math.max(1,bullet.number||0);
  switch(bullet.kind){
    case 'disc':return '•';case 'whiteCircle':return '◦';case 'blackSquare':return '▪';case 'whiteSquare':return '▫';
    case 'number':return `${bullet.number??1}.`;case 'alphabet':return `${letters(Math.min(n,18278))}.`;case 'upperAlphabet':return `${letters(Math.min(n,18278)).toUpperCase()}.`;
    case 'roman':return `${n<4000?roman(n):n}.`;case 'check':return bullet.checked?'☑':'☐';default:return '';
  }
}
export function argbToCss(value:string){if(!/^#[\da-f]{8}$/i.test(value))throw invalid();return '#'+value.slice(3)+value.slice(1,3);}
