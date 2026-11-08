// The theme this browser chose (System, Light or Dark), applied before the
// page draws so it doesn't flash. A classic script, loaded first in <head>;
// the switch in the header changes it.
(function () {
  var t = null;
  try { t = localStorage.getItem("mcast.theme"); } catch (e) {}
  if (t === "light" || t === "dark") {
    document.documentElement.dataset.theme = t;
    var m = document.querySelector('meta[name="color-scheme"]');
    if (m) m.content = t;
  }
})();
