// Dev-only: writes a static preview of every species × activity (not a test; run with `bun run preview -- <out.html>`).
import { renderToStaticMarkup } from "react-dom/server";
import { PET_ACTIVITIES } from "../src/index";
import { PET_CSS, PetBubble, PetSprite } from "../src/react/index";

const species = ["dog", "cat", "bird", "robot"] as const;
const rows = species
  .map(
    (sp) =>
      `<tr><th>${sp}</th>${[...PET_ACTIVITIES, "walk"]
        .map((a) =>
          renderToStaticMarkup(
            <td>
              <PetSprite species={sp} activity={a === "walk" ? "idle" : (a as never)} walking={a === "walk"} size={120} />
              <div className="lbl">{a}</div>
            </td>,
          ),
        )
        .join("")}</tr>`,
  )
  .join("");
const bubble = renderToStaticMarkup(
  <PetBubble
    view={{ petId: "p", agentId: "a", species: "dog", name: "Biscuit", activity: "alert", since: 0, mood: 70, bubble: { kind: "permission", text: "Woof! Need you! Run npm test?", requestId: "r" } }}
    onApprove={() => {}}
    onDeny={() => {}}
  />,
);
const html = `<!doctype html><html><head><meta charset="utf-8"><style>${PET_CSS}
body{font:12px system-ui;background:#eef2f7;margin:10px} td{text-align:center;padding:4px 6px;background:#fff;border-radius:8px} th{padding:0 8px} table{border-spacing:6px}
.lbl{color:#667;margin-top:2px} .paused .ap-pet *, .paused .ap-pet{animation-play-state:paused!important}</style></head>
<body><table>${rows}</table><div style="margin:30px 0 0 40px;width:240px">${bubble}</div></body></html>`;
await Bun.write(process.argv[2] ?? "preview.html", html);
console.log("wrote", process.argv[2]);
