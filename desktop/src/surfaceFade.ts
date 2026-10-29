// Fade newly shown surfaces in from script, one opacity step per frame.
// WebKitGTK runs CSS animations/transitions of opacity on a separate
// compositing layer; creating that layer flickers on Wayland. Setting the
// opacity from requestAnimationFrame is painted in place instead.
const SURFACES='.select-popup,.dropdown,.context-menu,.settings-modal,.modal-backdrop,.toast,.alert,.conflict-panel,.document-search,.selection-bar';
const DURATION_MS=130;
const ease=(t:number)=>1-Math.pow(1-t,3);

function fade(element:HTMLElement){
  if(element.dataset.faded)return;element.dataset.faded='1';
  const start=performance.now();element.style.opacity='0';
  const step=(now:number)=>{
    if(!element.isConnected)return;
    const t=Math.min(1,(now-start)/DURATION_MS);
    if(t<1){element.style.opacity=ease(t).toFixed(3);requestAnimationFrame(step);}else element.style.removeProperty('opacity');
  };
  requestAnimationFrame(step);
}

export function installSurfaceFade(root:Node=document.body){
  if(window.matchMedia?.('(prefers-reduced-motion: reduce)').matches)return;
  new MutationObserver(records=>{
    for(const record of records)for(const node of record.addedNodes){
      if(!(node instanceof HTMLElement))continue;
      if(node.matches(SURFACES))fade(node);
      node.querySelectorAll<HTMLElement>(SURFACES).forEach(fade);
    }
  }).observe(root,{childList:true,subtree:true});
}
