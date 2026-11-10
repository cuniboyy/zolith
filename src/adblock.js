// Runs in every frame before page scripts. Only acts on YouTube: its video ads come from the
// same servers as the videos, so the network filter can't stop them.
(() => {
  if (!/(^|\.)youtube(-nocookie)?\.com$/.test(location.hostname)) return;

  // 1. Strip ad descriptions out of player data before the player reads them.
  const AD_KEYS = ['adPlacements', 'adSlots', 'playerAds', 'adBreakHeartbeatParams'];
  const prune = (o) => {
    try {
      if (o && typeof o === 'object') {
        for (const k of AD_KEYS) if (k in o) delete o[k];
        if (o.playerResponse && typeof o.playerResponse === 'object') prune(o.playerResponse);
      }
    } catch (e) {}
    return o;
  };
  try {
    let initial;
    Object.defineProperty(window, 'ytInitialPlayerResponse', {
      configurable: true,
      get: () => initial,
      set: (v) => { initial = prune(v); },
    });
  } catch (e) {}
  const parse = JSON.parse;
  JSON.parse = function () { return prune(parse.apply(this, arguments)); };
  const json = Response.prototype.json;
  Response.prototype.json = function () { return json.apply(this, arguments).then(prune); };

  // 2. Hide ad slots in the page layout.
  const css = [
    'ytd-ad-slot-renderer', 'ytd-in-feed-ad-layout-renderer', 'ytd-promoted-sparkles-web-renderer',
    'ytd-display-ad-renderer', 'ytd-banner-promo-renderer', 'ytd-companion-slot-renderer',
    'ytd-action-companion-ad-renderer', 'ytd-player-legacy-desktop-watch-ads-renderer',
    '#masthead-ad', '#player-ads', '.ytp-ad-overlay-container', '.ytp-ad-message-container',
    'ytd-rich-item-renderer:has(ytd-ad-slot-renderer)',
    'ytd-rich-section-renderer:has(ytd-statement-banner-renderer)',
  ].map((s) => s + '{display:none!important}').join('\n');
  const addStyle = () => {
    const root = document.documentElement;
    if (!root) return false;
    const el = document.createElement('style');
    el.textContent = css;
    root.appendChild(el);
    return true;
  };
  if (!addStyle()) document.addEventListener('DOMContentLoaded', addStyle, { once: true });

  // 3. Fallback: if an ad still starts, jump to its end and press Skip.
  setInterval(() => {
    const player = document.querySelector('.html5-video-player.ad-showing');
    if (!player) return;
    const v = player.querySelector('video');
    if (v && isFinite(v.duration) && v.duration > 0 && v.currentTime < v.duration - 0.1) {
      v.currentTime = v.duration;
    }
    const skip = player.querySelector('.ytp-skip-ad-button, .ytp-ad-skip-button, .ytp-ad-skip-button-modern');
    if (skip) skip.click();
  }, 250);
})();
