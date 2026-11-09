(() => {
  const enableKeyboardScrolling = () => {
    document.querySelectorAll(".md-typeset pre > code, .rd-console pre").forEach((element) => {
      element.tabIndex = 0;
    });
    document.querySelectorAll(".md-typeset__scrollwrap").forEach((element) => {
      element.tabIndex = 0;
      element.setAttribute("role", "region");
      element.setAttribute("aria-label", "Scrollable data table");
    });
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", enableKeyboardScrolling, { once: true });
  } else {
    enableKeyboardScrolling();
  }
  if (typeof document$ !== "undefined") document$.subscribe(enableKeyboardScrolling);
})();
