// Hard fallback: never leave the splash stuck if startup hangs.
// Lives in a file rather than an inline <script> so the Content Security
// Policy can keep script-src at 'self'.
setTimeout(function () {
  var s = document.getElementById("splash");
  if (s) s.classList.add("hidden");
}, 8000);
