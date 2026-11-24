import { $, $$ } from "../page.js";

const norm = (s) =>
  s
    .toLowerCase()
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "");

function main() {
  const input = $("#gloss-filter");
  const clearBtn = $("#gloss-clear");
  const empty = $("#gloss-empty");
  const items = $$(".gloss-item");
  const sections = $$("section.gloss-letter");

  const applyFilter = () => {
    const q = norm(input.value.trim());
    let visibleCount = 0;

    for (const item of items) {
      const match = !q || norm(item.textContent).includes(q);
      item.hidden = !match;
      if (match) visibleCount += 1;
    }

    for (const sec of sections) {
      const hasVisible = sec.querySelector(".gloss-item:not([hidden])") !== null;
      sec.hidden = !hasVisible;
    }

    empty.hidden = visibleCount > 0;
  };

  input.addEventListener("input", applyFilter);
  clearBtn.addEventListener("click", () => {
    input.value = "";
    applyFilter();
    input.focus();
  });
}

main();
