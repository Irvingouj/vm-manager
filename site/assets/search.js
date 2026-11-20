// The documentation's search: an index built with the site (docs/search-index.json),
// loaded on first use and searched in the browser. No service is asked.
(function () {
  "use strict";
  var base = document.body.getAttribute("data-base") || "/";
  var input = document.getElementById("q");
  var box = document.getElementById("results");
  if (!input || !box) return;
  var entries = null;
  var loading = null;
  var selected = -1;

  function load() {
    if (!loading) {
      loading = fetch(base + "docs/search-index.json")
        .then(function (r) { return r.json(); })
        .then(function (data) {
          entries = data.map(function (e) {
            return { e: e, title: (e.p + " " + e.h).toLowerCase(), text: e.t.toLowerCase() };
          });
        })
        .catch(function () { entries = []; });
    }
    return loading;
  }

  function escapeHtml(s) {
    return s.replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  function mark(s, terms) {
    var out = escapeHtml(s);
    terms.forEach(function (t) {
      var re = new RegExp("(" + escapeHtml(t).replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + ")", "gi");
      out = out.replace(re, "<mark>$1</mark>");
    });
    return out;
  }

  function snippet(text, terms) {
    var lower = text.toLowerCase();
    var at = -1;
    for (var i = 0; i < terms.length && at < 0; i++) at = lower.indexOf(terms[i]);
    var start = Math.max(0, at - 50);
    var s = text.slice(start, start + 170);
    return (start > 0 ? "… " : "") + s + (start + 170 < text.length ? " …" : "");
  }

  function search(q) {
    var terms = q.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return [];
    var found = [];
    entries.forEach(function (x) {
      var score = 0;
      for (var i = 0; i < terms.length; i++) {
        var t = terms[i];
        var inTitle = x.title.indexOf(t) >= 0;
        var inText = x.text.indexOf(t) >= 0;
        if (!inTitle && !inText) return;
        score += (inTitle ? 10 : 0) + (inText ? 1 : 0);
      }
      if (x.text.indexOf(q.toLowerCase()) >= 0) score += 5;
      found.push({ x: x, score: score });
    });
    found.sort(function (a, b) { return b.score - a.score; });
    return found.slice(0, 12).map(function (f) { return { e: f.x.e, terms: terms }; });
  }

  function show() {
    var q = input.value.trim();
    selected = -1;
    if (!q) { box.hidden = true; input.setAttribute("aria-expanded", "false"); return; }
    load().then(function () {
      if (input.value.trim() !== q) return;
      var hits = search(q);
      if (!hits.length) {
        box.innerHTML = "<p>Nothing found for “" + escapeHtml(q) + "”.</p>";
      } else {
        box.innerHTML = hits.map(function (h) {
          var e = h.e;
          return '<a href="' + base + e.u + '"><span class="r-title">' + mark(e.h || e.p, h.terms) +
            (e.h ? ' <span class="r-page">· ' + escapeHtml(e.p) + "</span>" : "") +
            '</span><span class="r-text">' + mark(snippet(e.t, h.terms), h.terms) + "</span></a>";
        }).join("");
      }
      box.hidden = false;
      input.setAttribute("aria-expanded", "true");
    });
  }

  function move(d) {
    var links = box.querySelectorAll("a");
    if (!links.length) return;
    if (selected >= 0) links[selected].removeAttribute("aria-selected");
    selected = (selected + d + links.length) % links.length;
    links[selected].setAttribute("aria-selected", "true");
    links[selected].scrollIntoView({ block: "nearest" });
  }

  input.addEventListener("focus", load);
  input.addEventListener("input", show);
  input.addEventListener("keydown", function (ev) {
    if (ev.key === "ArrowDown") { ev.preventDefault(); move(1); }
    else if (ev.key === "ArrowUp") { ev.preventDefault(); move(-1); }
    else if (ev.key === "Enter") {
      var links = box.querySelectorAll("a");
      var a = links[selected >= 0 ? selected : 0];
      if (a) { ev.preventDefault(); window.location.href = a.href; }
    } else if (ev.key === "Escape") { input.value = ""; show(); input.blur(); }
  });
  document.addEventListener("keydown", function (ev) {
    if (ev.key === "/" && document.activeElement !== input && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName)) {
      ev.preventDefault();
      input.focus();
    }
  });
  document.addEventListener("click", function (ev) {
    if (!ev.target.closest || !ev.target.closest(".search")) box.hidden = true;
  });
})();
