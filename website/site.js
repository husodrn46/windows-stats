(() => {
 const locales=['tr','en'];
 const shotInfo={
 cpu:{file:'cpu.webp',tr:'01 / İşlemci geçmişi ve çekirdek kullanımı',en:'01 / CPU history and per-core usage'},
 memory:{file:'memory.webp',tr:'02 / Bellek geçmişi ve kullanım ayrıntıları',en:'02 / Memory history and usage details'},
 settings:{file:'settings.webp',tr:'03 / Windows Stats ayarları',en:'03 / Windows Stats settings'}
 };
 const image=document.querySelector('#shot'), caption=document.querySelector('#caption');
 let lang=locales.includes(new URL(location.href).searchParams.get('lang'))?new URL(location.href).searchParams.get('lang'):'tr', current='cpu';
 const shotButtons=[...document.querySelectorAll('[data-shot]')];
 const langButtons=[...document.querySelectorAll('[data-lang]')];
 function renderShot(){const item=shotInfo[current];let src='assets/'+item.file;if(image.getAttribute('src')!==src)image.src=src;image.alt=item[lang].slice(5);caption.textContent=item[lang];shotButtons.forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.shot===current)));}
 function renderLang(){document.documentElement.lang=lang;document.title=lang==='tr'?'Windows Stats · Görev çubuğunda sistem takibi':'Windows Stats · System monitoring in your taskbar';document.querySelectorAll('[data-tr][data-en]').forEach(e=>{e.textContent=e.dataset[lang]});langButtons.forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.lang===lang)));renderShot();}
 shotButtons.forEach(b=>b.addEventListener('click',()=>{current=b.dataset.shot;renderShot();}));
 langButtons.forEach(b=>b.addEventListener('click',()=>{lang=b.dataset.lang;const url=new URL(location.href);url.searchParams.set('lang',lang);history.replaceState(null,'',url);renderLang();}));
 renderLang();
})();
