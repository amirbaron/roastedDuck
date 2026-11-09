(() => {
  const prepareAccessibility = () => {
    document.querySelectorAll('.md-search[role="dialog"]').forEach((element) => {
      element.setAttribute("aria-label", "Search documentation");
    });
    document.querySelectorAll(".md-code__nav").forEach((element, index) => {
      element.setAttribute("aria-label", `Code block ${index + 1} actions`);
    });
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
    document.addEventListener("DOMContentLoaded", prepareAccessibility, { once: true });
  } else {
    prepareAccessibility();
  }
  if (typeof document$ !== "undefined") document$.subscribe(prepareAccessibility);
})();
