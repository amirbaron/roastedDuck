(() => {
  const explorer = document.getElementById("benchmark-explorer");
  if (!explorer) return;
  const output = document.getElementById("benchmark-output");
  const suite = document.getElementById("benchmark-suite");
  const metric = document.getElementById("benchmark-metric");
  fetch(explorer.dataset.source)
    .then(response => {
      if (!response.ok) throw new Error("Measurements unavailable");
      return response.json();
    })
    .then(data => {
      const render = () => {
        const [name, threads] = suite.value.split("-");
        const row = data.configurations.find(item => item.suite === name && item.threads === Number(threads));
        const key = { memory: "geomean_rust_cpp_whole_rss_ratio", time: "geomean_rust_cpp_best_time_ratio", total: "rust_cpp_sum_best_ratio" }[metric.value];
        const ratio = row[key];
        const maximum = Math.max(1, ratio) * 1.1;
        output.replaceChildren();
        for (const [label, value, isRust] of [["C++ reference", 1, false], ["roastedDuck", ratio, true]]) {
          const line = document.createElement("div");
          line.className = "rd-bar-row";
          const title = document.createElement("span"); title.textContent = label;
          const track = document.createElement("div"); track.className = "rd-bar-track";
          const bar = document.createElement("div"); bar.className = "rd-bar" + (isRust ? " rd-bar-rust" : "");
          bar.style.width = `${value / maximum * 100}%`; track.append(bar);
          const number = document.createElement("span"); number.textContent = `${value.toFixed(3)}×`;
          line.append(title, track, number); output.append(line);
        }
        const note = document.createElement("p"); note.className = "rd-benchmark-note";
        const percent = Math.abs((ratio - 1) * 100).toFixed(1);
        note.textContent = metric.value === "memory"
          ? `${percent}% lower geometric-mean whole-process peak RSS. This is not a maximum-memory guarantee.`
          : `${percent}% ${ratio >= 1 ? "longer" : "shorter"} execution time for the selected aggregate. See individual queries below.`;
        output.append(note);
      };
      suite.addEventListener("change", render); metric.addEventListener("change", render); render();
    })
    .catch(() => { output.textContent = "The explorer could not load. Use the table and downloadable evidence on this page."; });
})();
