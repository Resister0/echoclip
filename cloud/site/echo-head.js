// the kit's head lines (the hero's entrance and the moments' guard) as a file: the CSP runs script files only
document.documentElement.classList.add('m-enter', 'mo-js');
setTimeout(() => document.documentElement.classList.add('m-enter-done'), 700);
setTimeout(() => document.documentElement.classList.add('mo-safe'), 2500);
// big screens (11 Nov 2026, the user: 1080p and 1440p must look like the same design): the page is laid out for a
// 1600x900 screen and scaled up from there, so 1920x1080 and 2560x1440 show the same page, only sharper.
// zoom = the smaller of width/1600 and height/900, never under 1 (laptops and phones keep the normal layout)
(() => {
  const root = document.documentElement;
  const fit = () => {
    const z = Math.min(innerWidth / 1600, innerHeight / 900);
    if (z > 1.04) { root.style.zoom = z.toFixed(3); root.style.setProperty('--z', z.toFixed(3)); root.dataset.zoom = ''; } else { root.style.zoom = ''; root.style.removeProperty('--z'); delete root.dataset.zoom; }
  };
  fit(); addEventListener('resize', fit);
})();
