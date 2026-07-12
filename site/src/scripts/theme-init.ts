// No-FOUC theme bootstrap. Imported as `?raw` and inlined (is:inline, NOT a
// module, NOT deferred) in the <head> of every page so data-theme is set BEFORE
// first paint. Latte is the default; Mocha when stored OR prefers-color-scheme
// dark. Mirrors the app's Latte-default + Catppuccin position.
(function () {
  try {
    var stored = localStorage.getItem('onote-theme');
    var theme =
      stored === 'latte' || stored === 'mocha'
        ? stored
        : window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
          ? 'mocha'
          : 'latte';
    document.documentElement.setAttribute('data-theme', theme);
  } catch (_) {
    document.documentElement.setAttribute('data-theme', 'latte');
  }
})();
