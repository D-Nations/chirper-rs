// Replaces the inline onclick="return confirm(...)" handler so a strict
// Content-Security-Policy (no inline script) can be enforced.
document.addEventListener('click', function (e) {
  var el = e.target.closest('[data-confirm]');
  if (el && !window.confirm(el.getAttribute('data-confirm'))) e.preventDefault();
});
