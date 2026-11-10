(() => {
  const keyboardScrollTargets = new WeakSet();
  const enableKeyboardScroll = (element) => {
    element.tabIndex = 0;
    if (keyboardScrollTargets.has(element)) return;
    keyboardScrollTargets.add(element);
    element.addEventListener("keydown", (event) => {
      if (event.target !== element || event.defaultPrevented || event.altKey ||
          event.ctrlKey || event.metaKey || event.shiftKey) return;
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      if (element.scrollWidth <= element.clientWidth) return;
      const before = element.scrollLeft;
      element.scrollLeft += event.key === "ArrowRight" ? 40 : -40;
      if (element.scrollLeft !== before) event.preventDefault();
    });
  };
  const prepareAccessibility = () => {
    document.querySelectorAll('.md-search[role="dialog"]').forEach((element) => {
      element.setAttribute("aria-label", "Search documentation");
    });
    document.querySelectorAll(".md-code__nav").forEach((element, index) => {
      element.setAttribute("aria-label", `Code block ${index + 1} actions`);
    });
    document.querySelectorAll(".md-typeset pre > code, .rd-console pre").forEach((element) => {
      enableKeyboardScroll(element);
    });
    document.querySelectorAll(".md-typeset__scrollwrap").forEach((element) => {
      enableKeyboardScroll(element);
      element.setAttribute("role", "region");
      element.setAttribute("aria-label", "Scrollable data table");
    });
  };
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", prepareAccessibility, { once: true });
  } else {
    prepareAccessibility();
  }
  if (typeof document$ !== "undefined") document$.subscribe(prepareAccessibility);
})();
