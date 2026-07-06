// Client renderer for /stats/. Fetches the public stats snapshot from R2 and
// renders every number and chart client-side, so the static page never goes stale.

const STATS_URL = "https://assets.herdr.dev/stats/stats.json";
const REPO_API_URL = "https://api.github.com/repos/ogulcancelik/herdr";

const LINE_H = 320;
const BAR_W = 420;
const BAR_H = 240;
const STAR_MILESTONES = [1_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000];
const MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

type SeriesPoint = { d: string; v: number };

type StatsJson = {
  schemaVersion: number;
  generatedAt: string;
  repo: string;
  bornDay: string;
  stars: { total: number; daily: SeriesPoint[] };
  downloads: { github: number; brew: number; daily: SeriesPoint[] };
  releases: { shipped: number };
  traffic: {
    windowDays: number;
    daily: SeriesPoint[];
    topCountries: Array<{ code: string; visits: number }>;
    topReferrers: Array<{ host: string; visits: number }>;
    topPaths: Array<{ path: string; views: number }>;
  };
  timeline: Array<{ d: string; label: string; note?: string; badge?: string }>;
};

type WeekPoint = { weekStart: string; value: number; partial: boolean; daysIn: number };
type Milestone = { day: string; value: number; label: string };

// ---------- formatting ----------

function fmtInt(value: number): string {
  return Math.round(value).toLocaleString("en-US");
}

function fmtCompact(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value % 1_000_000 === 0 ? 0 : 1)}M`;
  if (value >= 10_000) return `${Math.round(value / 1_000)}k`;
  if (value >= 1_000) {
    const scaled = value / 1_000;
    return `${Number.isInteger(scaled) ? scaled : scaled.toFixed(1)}k`;
  }
  return String(Math.round(value));
}

function fmtDay(day: string): string {
  const date = new Date(`${day}T00:00:00Z`);
  return `${MONTHS[date.getUTCMonth()]} ${date.getUTCDate()}`;
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

// ---------- date/series math ----------

function dayEpoch(day: string): number {
  return Date.parse(`${day}T00:00:00Z`);
}

function daysBetween(fromDay: string, toDay: string): number {
  return Math.round((dayEpoch(toDay) - dayEpoch(fromDay)) / 86_400_000);
}

function todayUtc(): string {
  return new Date().toISOString().slice(0, 10);
}

function weekStartOf(day: string): string {
  const date = new Date(`${day}T00:00:00Z`);
  const sinceMonday = (date.getUTCDay() + 6) % 7;
  return new Date(date.getTime() - sinceMonday * 86_400_000).toISOString().slice(0, 10);
}

function weekMeta(weekStart: string, lastDataDay: string | null): { partial: boolean; daysIn: number } {
  if (!lastDataDay) return { partial: false, daysIn: 7 };
  const weekEnd = new Date(dayEpoch(weekStart) + 6 * 86_400_000).toISOString().slice(0, 10);
  const partial = lastDataDay < weekEnd || weekEnd >= todayUtc();
  const daysIn = partial ? Math.min(daysBetween(weekStart, lastDataDay) + 1, 7) : 7;
  return { partial, daysIn };
}

function deltasFromCumulative(daily: SeriesPoint[]): SeriesPoint[] {
  const deltas: SeriesPoint[] = [];
  for (let index = 1; index < daily.length; index += 1) {
    deltas.push({ d: daily[index].d, v: Math.max(daily[index].v - daily[index - 1].v, 0) });
  }
  return deltas;
}

function weeklySums(days: SeriesPoint[], lastDataDay: string | null): WeekPoint[] {
  const byWeek = new Map<string, number>();
  for (const point of days) {
    const week = weekStartOf(point.d);
    byWeek.set(week, (byWeek.get(week) ?? 0) + point.v);
  }
  return [...byWeek.entries()]
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([weekStart, value]) => ({ weekStart, value, ...weekMeta(weekStart, lastDataDay) }));
}

function weeklyAverages(days: SeriesPoint[], lastDataDay: string | null): WeekPoint[] {
  const byWeek = new Map<string, { sum: number; count: number }>();
  for (const point of days) {
    const week = weekStartOf(point.d);
    const bucket = byWeek.get(week) ?? { sum: 0, count: 0 };
    bucket.sum += point.v;
    bucket.count += 1;
    byWeek.set(week, bucket);
  }
  return [...byWeek.entries()]
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([weekStart, bucket]) => ({
      weekStart,
      value: Math.round(bucket.sum / bucket.count),
      ...weekMeta(weekStart, lastDataDay),
    }));
}

function starMilestones(daily: SeriesPoint[]): Milestone[] {
  const milestones: Milestone[] = [];
  let next = 0;
  for (const point of daily) {
    while (next < STAR_MILESTONES.length && point.v >= STAR_MILESTONES[next]) {
      const value = STAR_MILESTONES[next];
      milestones.push({ day: point.d, value, label: value >= 1_000 ? `${value / 1_000}k` : String(value) });
      next += 1;
    }
  }
  return milestones;
}

function niceTicks(max: number, count = 4): number[] {
  if (max <= 0) return [0];
  const rawStep = max / count;
  const power = 10 ** Math.floor(Math.log10(rawStep));
  const step = [1, 2, 2.5, 5, 10].map((mult) => mult * power).find((candidate) => candidate >= rawStep) ?? rawStep;
  const ticks: number[] = [];
  for (let tick = 0; tick <= max; tick += step) ticks.push(tick);
  return ticks;
}

function monthStartsBetween(firstDay: string, lastDay: string): string[] {
  const starts: string[] = [];
  const last = dayEpoch(lastDay);
  const cursor = new Date(`${firstDay.slice(0, 7)}-01T00:00:00Z`);
  cursor.setUTCMonth(cursor.getUTCMonth() + 1);
  while (cursor.getTime() <= last) {
    starts.push(cursor.toISOString().slice(0, 10));
    cursor.setUTCMonth(cursor.getUTCMonth() + 1);
  }
  return starts;
}

// ---------- chart builders (SVG strings) ----------

type LineChartOptions = {
  points: SeriesPoint[];
  colorVar: string;
  unit: string;
  width: number;
  milestones?: Milestone[];
  events?: Array<{ day: string; label: string }>;
};

function lineChart(options: LineChartOptions): string {
  const { points, width } = options;
  if (points.length < 2) return `<p class="chart-empty">not enough data yet</p>`;

  const margin = { top: 34, right: 26, bottom: 30, left: 54 };
  const innerW = width - margin.left - margin.right;
  const innerH = LINE_H - margin.top - margin.bottom;

  const x0 = dayEpoch(points[0].d);
  const x1 = dayEpoch(points[points.length - 1].d);
  const yMaxData = Math.max(...points.map((point) => point.v));
  const ticks = niceTicks(yMaxData);
  const yMax = Math.max(ticks[ticks.length - 1], yMaxData) * 1.04;

  const xFor = (day: string): number => margin.left + ((dayEpoch(day) - x0) / (x1 - x0)) * innerW;
  const yFor = (value: number): number => margin.top + innerH - (value / yMax) * innerH;

  const coords = points.map((point) => ({ x: xFor(point.d), y: yFor(point.v), day: point.d, value: point.v }));
  const linePath = coords.map((c, i) => `${i === 0 ? "M" : "L"}${c.x.toFixed(1)},${c.y.toFixed(1)}`).join("");
  const baseline = margin.top + innerH;
  const areaPath = `${linePath}L${coords[coords.length - 1].x.toFixed(1)},${baseline}L${coords[0].x.toFixed(1)},${baseline}Z`;

  const grid = ticks
    .map((tick) => {
      const y = yFor(tick).toFixed(1);
      return `<line class="grid" x1="${margin.left}" y1="${y}" x2="${width - margin.right}" y2="${y}"/>` +
        `<text class="tick" x="${margin.left - 8}" y="${y}" dy="0.32em" text-anchor="end">${fmtCompact(tick)}</text>`;
    })
    .join("");

  const monthLabels = monthStartsBetween(points[0].d, points[points.length - 1].d)
    .map((day) => {
      const x = xFor(day).toFixed(1);
      return `<text class="tick" x="${x}" y="${baseline + 20}" text-anchor="middle">${MONTHS[new Date(`${day}T00:00:00Z`).getUTCMonth()]}</text>`;
    })
    .join("");

  const milestoneMarks = (options.milestones ?? [])
    .map((milestone) => {
      const x = xFor(milestone.day);
      const y = yFor(milestone.value);
      return (
        `<line class="milestone-leader" x1="${x.toFixed(1)}" y1="${(y - 9).toFixed(1)}" x2="${x.toFixed(1)}" y2="${(y - 18).toFixed(1)}"/>` +
        `<circle class="dot" cx="${x.toFixed(1)}" cy="${y.toFixed(1)}" r="4.5" style="fill:var(${options.colorVar})"/>` +
        `<text class="milestone-label" x="${x.toFixed(1)}" y="${(y - 23).toFixed(1)}" text-anchor="middle">${milestone.label}</text>`
      );
    })
    .join("");

  const events = (options.events ?? []).map((event) => {
    const x = xFor(event.day);
    const nearest = coords.reduce((best, c) => (Math.abs(c.x - x) < Math.abs(best.x - x) ? c : best), coords[0]);
    return { ...event, dotX: nearest.x, dotY: nearest.y };
  });
  const eventBaseX = Math.min(...events.map((event) => event.dotX), Infinity) - 12;
  const eventMarks = events
    .map((event, index) => {
      const labelY = margin.top + 14 + index * 16;
      return (
        `<line class="milestone-leader" x1="${(eventBaseX + 4).toFixed(1)}" y1="${(labelY + 4).toFixed(1)}" x2="${(event.dotX - 2).toFixed(1)}" y2="${(event.dotY - 6).toFixed(1)}"/>` +
        `<circle class="event-dot" cx="${event.dotX.toFixed(1)}" cy="${event.dotY.toFixed(1)}" r="4" style="stroke:var(${options.colorVar})"/>` +
        `<text class="milestone-label" x="${eventBaseX.toFixed(1)}" y="${labelY.toFixed(1)}" text-anchor="end">${escapeHtml(event.label)}</text>`
      );
    })
    .join("");

  const end = coords[coords.length - 1];
  const endDot = `<circle class="dot" cx="${end.x.toFixed(1)}" cy="${end.y.toFixed(1)}" r="4.5" style="fill:var(${options.colorVar})"/>`;

  // escape < so the JSON can never terminate the wrapping <script> tag
  const data = JSON.stringify({
    unit: options.unit,
    width,
    points: coords.map((c) => ({ x: Number(c.x.toFixed(1)), y: Number(c.y.toFixed(1)), day: c.day, value: c.value })),
  }).replace(/</g, "\\u003c");

  return `
    <div class="chart">
      <svg viewBox="0 0 ${width} ${LINE_H}" role="img" aria-label="${options.unit} over time" tabindex="0">
        ${grid}
        ${monthLabels}
        <path class="area" d="${areaPath}" style="fill:var(${options.colorVar})"/>
        <path class="line" d="${linePath}" style="stroke:var(${options.colorVar})"/>
        ${milestoneMarks}
        ${eventMarks}
        ${endDot}
        <line class="crosshair" y1="${margin.top}" y2="${baseline}" x1="-10" x2="-10" hidden></line>
        <circle class="hover-dot" r="4.5" cx="-10" cy="-10" style="fill:var(${options.colorVar})" hidden></circle>
      </svg>
      <div class="chart-tip" hidden></div>
      <script type="application/json">${data}</script>
    </div>`;
}

type BarChartOptions = {
  weeks: WeekPoint[];
  colorVar: string;
  unit: string;
  maxWeeks?: number;
};

function barChart(options: BarChartOptions): string {
  const weeks = options.weeks.slice(-(options.maxWeeks ?? 14));
  if (weeks.length === 0) return `<p class="chart-empty">not enough data yet</p>`;

  const margin = { top: 26, right: 12, bottom: 30, left: 44 };
  const innerW = BAR_W - margin.left - margin.right;
  const innerH = BAR_H - margin.top - margin.bottom;

  const yMaxData = Math.max(...weeks.map((week) => week.value));
  const ticks = niceTicks(yMaxData, 3);
  const yMax = Math.max(ticks[ticks.length - 1], yMaxData) * 1.06;
  const yFor = (value: number): number => margin.top + innerH - (value / yMax) * innerH;
  const baseline = margin.top + innerH;

  const band = innerW / weeks.length;
  const barWidth = Math.min(24, Math.max(band - 2, 4));

  const grid = ticks
    .filter((tick) => tick > 0)
    .map((tick) => {
      const y = yFor(tick).toFixed(1);
      return `<line class="grid" x1="${margin.left}" y1="${y}" x2="${BAR_W - margin.right}" y2="${y}"/>` +
        `<text class="tick" x="${margin.left - 7}" y="${y}" dy="0.32em" text-anchor="end">${fmtCompact(tick)}</text>`;
    })
    .join("");

  const maxValue = Math.max(...weeks.filter((week) => !week.partial).map((week) => week.value), 0);
  const labelStep = Math.ceil(weeks.length / 5);

  const bars = weeks
    .map((week, index) => {
      const xCenter = margin.left + band * index + band / 2;
      const x = xCenter - barWidth / 2;
      const yTop = yFor(week.value);
      const r = Math.min(4, barWidth / 2, Math.max(baseline - yTop, 0));
      const path =
        `M${x.toFixed(1)},${baseline}` +
        `L${x.toFixed(1)},${(yTop + r).toFixed(1)}` +
        `Q${x.toFixed(1)},${yTop.toFixed(1)} ${(x + r).toFixed(1)},${yTop.toFixed(1)}` +
        `L${(x + barWidth - r).toFixed(1)},${yTop.toFixed(1)}` +
        `Q${(x + barWidth).toFixed(1)},${yTop.toFixed(1)} ${(x + barWidth).toFixed(1)},${(yTop + r).toFixed(1)}` +
        `L${(x + barWidth).toFixed(1)},${baseline}Z`;
      const capLabel = !week.partial && week.value === maxValue && week.value > 0
        ? `<text class="cap-label" x="${xCenter.toFixed(1)}" y="${(yTop - 7).toFixed(1)}" text-anchor="middle">${fmtCompact(week.value)}</text>`
        : "";
      const axisLabel = index % labelStep === 0
        ? `<text class="tick" x="${xCenter.toFixed(1)}" y="${baseline + 20}" text-anchor="middle">${fmtDay(week.weekStart)}</text>`
        : "";
      const tipText = week.partial
        ? `week of ${fmtDay(week.weekStart)} · ${fmtInt(week.value)} ${options.unit} · day ${week.daysIn}/7`
        : `week of ${fmtDay(week.weekStart)} · ${fmtInt(week.value)} ${options.unit}`;
      return (
        `<g class="bar${week.partial ? " partial" : ""}" data-tip="${escapeHtml(tipText)}">` +
        `<rect class="hit" x="${(margin.left + band * index).toFixed(1)}" y="${margin.top}" width="${band.toFixed(1)}" height="${innerH}"></rect>` +
        `<path class="fill" d="${path}" style="fill:var(${options.colorVar})"/>` +
        `</g>${capLabel}${axisLabel}`
      );
    })
    .join("");

  return `
    <div class="chart">
      <svg viewBox="0 0 ${BAR_W} ${BAR_H}" role="img" aria-label="${options.unit} per week">
        ${grid}
        <line class="grid strong" x1="${margin.left}" y1="${baseline}" x2="${BAR_W - margin.right}" y2="${baseline}"/>
        ${bars}
      </svg>
      <div class="chart-tip" hidden></div>
    </div>`;
}

// ---------- tables, rank lists, timeline ----------

function detailsTable(headers: [string, string], rows: string): string {
  return `
    <details class="chart-table">
      <summary>view as table</summary>
      <div class="chart-table-scroll">
        <table><thead><tr><th>${headers[0]}</th><th>${headers[1]}</th></tr></thead><tbody>${rows}</tbody></table>
      </div>
    </details>`;
}

function weeklyTable(weeks: WeekPoint[], unit: string): string {
  const rows = [...weeks]
    .slice(-14)
    .reverse()
    .map(
      (week) =>
        `<tr><td>week of ${fmtDay(week.weekStart)}</td><td>${fmtInt(week.value)}${week.partial ? ` (day ${week.daysIn}/7)` : ""}</td></tr>`,
    )
    .join("");
  return detailsTable(["week", unit], rows);
}

function cumulativeTable(points: SeriesPoint[], unit: string): string {
  const byMonth = new Map<string, SeriesPoint>();
  for (const point of points) byMonth.set(point.d.slice(0, 7), point);
  const rows = [...byMonth.values()]
    .map((point) => `<tr><td>${fmtDay(point.d)}</td><td>${fmtInt(point.v)}</td></tr>`)
    .join("");
  return detailsTable(["end of month", unit], rows);
}

function flagEmoji(code: string): string {
  if (!/^[A-Z]{2}$/.test(code)) return "";
  return String.fromCodePoint(...[...code].map((char) => 0x1f1a5 + char.charCodeAt(0)));
}

function countryName(code: string): string {
  try {
    return new Intl.DisplayNames(["en"], { type: "region" }).of(code) ?? code;
  } catch {
    return code;
  }
}

function rankList(rows: Array<{ label: string; value: number }>, unit: string): string {
  if (rows.length === 0) return `<p class="chart-empty">no data yet</p>`;
  const max = Math.max(...rows.map((row) => row.value), 1);
  const items = rows
    .map(
      (row) =>
        `<div class="rank-row" title="${escapeHtml(row.label)} · ${fmtInt(row.value)} ${unit}">` +
        `<span class="rank-name">${escapeHtml(row.label)}</span>` +
        `<span class="rank-bar"><span class="rank-fill" style="width:${((row.value / max) * 100).toFixed(1)}%"></span></span>` +
        `<span class="rank-value">${fmtCompact(row.value)}</span>` +
        `</div>`,
    )
    .join("");
  return `<div class="rank-list">${items}</div>`;
}

function timelineTerminal(stats: StatsJson, milestones: Milestone[]): string {
  const lines: Array<{ day: string; text: string; note: string }> = [];
  for (const entry of stats.timeline) {
    const note = entry.note ?? `day ${daysBetween(stats.bornDay, entry.d)}`;
    lines.push({ day: entry.d, text: escapeHtml(entry.label), note: escapeHtml(note) });
  }
  let previousDay = stats.timeline.find((entry) => entry.label.includes("first star"))?.d ?? stats.bornDay;
  for (const milestone of milestones) {
    lines.push({ day: milestone.day, text: `★ ${fmtInt(milestone.value)}`, note: `+${daysBetween(previousDay, milestone.day)} days` });
    previousDay = milestone.day;
  }
  lines.sort((a, b) => a.day.localeCompare(b.day));
  lines.push({ day: todayUtc(), text: `★ ${fmtInt(stats.stars.total)}`, note: "today" });

  const body = lines
    .map(
      (line) =>
        `<div class="term-line"><span class="term-date">${fmtDay(line.day)}</span><span class="term-text">${line.text}</span><span class="term-note">${line.note}</span></div>`,
    )
    .join("");

  return `
    <div class="milestone-term" role="img" aria-label="herdr timeline">
      <div class="term-line term-cmd-line"><span class="term-prompt">$</span><span class="term-cmd">herdr stats --timeline</span></div>
      ${body}
    </div>`;
}

// ---------- interaction ----------

function attachChartInteractions(root: ParentNode): void {
  root.querySelectorAll<HTMLElement>(".chart").forEach((chart) => {
    const dataEl = chart.querySelector<HTMLScriptElement>("script[type='application/json']");
    const tip = chart.querySelector<HTMLElement>(".chart-tip");
    const svg = chart.querySelector<SVGSVGElement>("svg");
    if (!svg || !tip) return;

    if (dataEl?.textContent) {
      const data = JSON.parse(dataEl.textContent) as {
        unit: string;
        width: number;
        points: Array<{ x: number; y: number; day: string; value: number }>;
      };
      const crosshair = svg.querySelector<SVGLineElement>(".crosshair");
      const hoverDot = svg.querySelector<SVGCircleElement>(".hover-dot");
      if (!crosshair || !hoverDot) return;
      const points = data.points;
      let active = -1;

      const showIndex = (index: number): void => {
        if (index < 0 || index >= points.length) return;
        active = index;
        const point = points[index];
        crosshair.setAttribute("x1", String(point.x));
        crosshair.setAttribute("x2", String(point.x));
        crosshair.hidden = false;
        hoverDot.setAttribute("cx", String(point.x));
        hoverDot.setAttribute("cy", String(point.y));
        hoverDot.hidden = false;
        tip.innerHTML = "";
        const strong = document.createElement("strong");
        strong.textContent = `${point.value.toLocaleString("en-US")} ${data.unit}`;
        tip.appendChild(strong);
        tip.appendChild(document.createElement("br"));
        tip.appendChild(document.createTextNode(fmtDay(point.day)));
        const rect = svg.getBoundingClientRect();
        const scale = rect.width / data.width;
        tip.style.left = `${point.x * scale}px`;
        tip.style.top = `${point.y * scale}px`;
        tip.hidden = false;
      };

      const hide = (): void => {
        active = -1;
        crosshair.hidden = true;
        hoverDot.hidden = true;
        tip.hidden = true;
      };

      svg.addEventListener("pointermove", (event) => {
        const rect = svg.getBoundingClientRect();
        const x = ((event.clientX - rect.left) / rect.width) * data.width;
        let best = 0;
        let bestDist = Infinity;
        for (let i = 0; i < points.length; i += 1) {
          const dist = Math.abs(points[i].x - x);
          if (dist < bestDist) {
            bestDist = dist;
            best = i;
          }
        }
        showIndex(best);
      });
      svg.addEventListener("pointerleave", hide);
      svg.addEventListener("focus", () => showIndex(points.length - 1));
      svg.addEventListener("blur", hide);
      svg.addEventListener("keydown", (event) => {
        if (event.key === "ArrowLeft") {
          showIndex(Math.max(active - 1, 0));
          event.preventDefault();
        }
        if (event.key === "ArrowRight") {
          showIndex(Math.min(active + 1, points.length - 1));
          event.preventDefault();
        }
      });
    }

    chart.querySelectorAll<SVGGElement>(".bar").forEach((bar) => {
      bar.addEventListener("pointerenter", () => {
        const hit = bar.querySelector(".hit");
        if (!hit) return;
        const svgRect = svg.getBoundingClientRect();
        const barRect = hit.getBoundingClientRect();
        tip.textContent = bar.getAttribute("data-tip");
        tip.style.left = `${barRect.left - svgRect.left + barRect.width / 2}px`;
        tip.style.top = `${barRect.top - svgRect.top + 18}px`;
        tip.hidden = false;
        bar.classList.add("active");
      });
      bar.addEventListener("pointerleave", () => {
        tip.hidden = true;
        bar.classList.remove("active");
      });
    });
  });
}

// ---------- live star counter ----------

function startLiveStars(initial: number): void {
  const hero = document.getElementById("star-count");
  if (!hero) return;
  let current = initial;
  const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  const renderCount = (value: number): void => {
    hero.textContent = Math.round(value).toLocaleString("en-US");
  };
  renderCount(current);

  const animateTo = (target: number): void => {
    if (reduceMotion || target <= current) {
      current = Math.max(current, target);
      renderCount(current);
      return;
    }
    const from = current;
    const start = performance.now();
    const duration = 900;
    const frame = (now: number): void => {
      const t = Math.min((now - start) / duration, 1);
      const eased = 1 - (1 - t) ** 3;
      renderCount(from + (target - from) * eased);
      if (t < 1) requestAnimationFrame(frame);
      else current = target;
    };
    requestAnimationFrame(frame);
  };

  const poll = (): void => {
    fetch(REPO_API_URL)
      .then((res) => (res.ok ? res.json() : null))
      .then((repo) => {
        if (repo && typeof repo.stargazers_count === "number") animateTo(repo.stargazers_count);
      })
      .catch(() => {
        // keep last known number; the counter must never break the page
      });
  };
  poll();
  setInterval(poll, 60_000);
}

// ---------- page assembly ----------

function setText(id: string, text: string): void {
  const el = document.getElementById(id);
  if (el) el.textContent = text;
}

function setHtml(id: string, html: string): void {
  const el = document.getElementById(id);
  if (el) el.innerHTML = html;
}

function render(stats: StatsJson): void {
  const starsDaily = stats.stars.daily;
  const lastStarDay = starsDaily.at(-1)?.d ?? null;
  const milestones = starMilestones(starsDaily);
  const badges = stats.timeline
    .filter((entry) => entry.badge)
    .map((entry) => ({ day: entry.d, label: entry.badge ?? "" }));

  // hero
  const last7d = (() => {
    const last = starsDaily.at(-1);
    if (!last) return null;
    const weekAgoDay = new Date(dayEpoch(last.d) - 7 * 86_400_000).toISOString().slice(0, 10);
    const anchor = [...starsDaily].reverse().find((point) => point.d <= weekAgoDay);
    return anchor ? stats.stars.total - anchor.v : null;
  })();
  if (last7d !== null) setText("hero-note", `+${fmtInt(last7d)} stars in the last 7 days`);
  if (starsDaily[0]) setText("stars-since", `since ${fmtDay(starsDaily[0].d)}`);
  setHtml(
    "chart-stars",
    lineChart({ points: starsDaily, colorVar: "--accent", unit: "stars", width: 640, milestones, events: badges }) +
      cumulativeTable(starsDaily, "stars"),
  );

  // KPIs
  const downloadsDeltas = deltasFromCumulative(stats.downloads.daily);
  const lastDownloadDay = stats.downloads.daily.at(-1)?.d ?? null;
  const downloadsWeekly = weeklySums(downloadsDeltas, lastDownloadDay);
  const lastFullWeek = [...downloadsWeekly].reverse().find((week) => !week.partial);
  setText("kpi-installs", fmtInt(stats.downloads.github + stats.downloads.brew));
  setHtml(
    "kpi-installs-sub",
    `${lastFullWeek ? `<span class="up">+${fmtInt(lastFullWeek.value)}</span> last week · ` : ""}releases + homebrew`,
  );

  const trafficDaily = stats.traffic.daily;
  const lastSevenTraffic = trafficDaily.slice(-7);
  const visitorsAvg = lastSevenTraffic.length
    ? Math.round(lastSevenTraffic.reduce((sum, point) => sum + point.v, 0) / lastSevenTraffic.length)
    : null;
  setText("kpi-visitors", visitorsAvg === null ? "—" : fmtCompact(visitorsAvg));

  setText("kpi-releases", fmtInt(stats.releases.shipped));
  setText("kpi-age", fmtInt(daysBetween(stats.bornDay, todayUtc())));
  setText("kpi-age-sub", `born ${fmtDay(stats.bornDay)}, ${stats.bornDay.slice(0, 4)}`);

  // weekly velocity
  const starsDeltas = deltasFromCumulative(starsDaily);
  setHtml(
    "chart-stars-week",
    barChart({ weeks: weeklySums(starsDeltas, lastStarDay), colorVar: "--accent", unit: "stars" }) +
      weeklyTable(weeklySums(starsDeltas, lastStarDay), "stars"),
  );
  setHtml(
    "chart-downloads-week",
    barChart({ weeks: downloadsWeekly, colorVar: "--green", unit: "downloads" }) + weeklyTable(downloadsWeekly, "downloads"),
  );
  const visitorsWeekly = weeklyAverages(trafficDaily, trafficDaily.at(-1)?.d ?? null);
  setHtml(
    "chart-visitors-week",
    barChart({ weeks: visitorsWeekly, colorVar: "--yellow", unit: "avg daily visitors" }) +
      weeklyTable(visitorsWeekly, "avg daily visitors"),
  );

  // downloads cumulative
  const trackedSince = stats.downloads.daily[0]?.d;
  setText(
    "downloads-sub",
    `GitHub release downloads, cumulative${trackedSince ? `, tracked since ${fmtDay(trackedSince)}` : ""}. ` +
      `Homebrew adds ${fmtInt(stats.downloads.brew)} installs on top (not shown in the curve).`,
  );
  setHtml(
    "chart-downloads",
    lineChart({ points: stats.downloads.daily, colorVar: "--green", unit: "downloads", width: 960 }) +
      cumulativeTable(stats.downloads.daily, "downloads"),
  );

  // traffic
  setText("traffic-sub", `herdr.dev traffic over the last ${stats.traffic.windowDays} days, measured by Cloudflare.`);
  setHtml(
    "rank-countries",
    rankList(
      stats.traffic.topCountries.map((row) => ({ label: `${flagEmoji(row.code)} ${countryName(row.code)}`.trim(), value: row.visits })),
      "visits",
    ),
  );
  setHtml(
    "rank-referrers",
    rankList(stats.traffic.topReferrers.map((row) => ({ label: row.host, value: row.visits })), "visits"),
  );
  setHtml(
    "rank-paths",
    rankList(stats.traffic.topPaths.map((row) => ({ label: row.path, value: row.views })), "page views"),
  );

  // timeline + freshness
  setHtml("timeline", timelineTerminal(stats, milestones));
  const generated = new Date(stats.generatedAt);
  const ageHours = (Date.now() - generated.getTime()) / 3_600_000;
  const stampText = `Data updated ${fmtDay(stats.generatedAt.slice(0, 10))}, ${stats.generatedAt.slice(11, 16)} UTC.`;
  setText("updated-note", stampText);
  if (ageHours > 6) {
    const note = document.createElement("p");
    note.className = "stats-stale";
    note.textContent = `note: snapshot is ${Math.round(ageHours)} hours old — the star counter above is still live.`;
    document.getElementById("sources-note")?.after(note);
  }

  attachChartInteractions(document);
  startLiveStars(stats.stars.total);
}

function assertStatsShape(stats: StatsJson): void {
  const ok =
    stats.schemaVersion === 1 &&
    typeof stats.generatedAt === "string" &&
    typeof stats.bornDay === "string" &&
    typeof stats.stars?.total === "number" &&
    Array.isArray(stats.stars?.daily) &&
    typeof stats.downloads?.github === "number" &&
    typeof stats.downloads?.brew === "number" &&
    Array.isArray(stats.downloads?.daily) &&
    typeof stats.releases?.shipped === "number" &&
    Array.isArray(stats.traffic?.daily) &&
    Array.isArray(stats.traffic?.topCountries) &&
    Array.isArray(stats.traffic?.topReferrers) &&
    Array.isArray(stats.traffic?.topPaths) &&
    Array.isArray(stats.timeline);
  if (!ok) throw new Error("stats snapshot has unexpected shape");
}

export function initStatsPage(): void {
  fetch(STATS_URL, { headers: { accept: "application/json" } })
    .then((res) => {
      if (!res.ok) throw new Error(`stats fetch failed: ${res.status}`);
      return res.json() as Promise<StatsJson>;
    })
    .then((stats) => {
      assertStatsShape(stats);
      render(stats);
    })
    .catch((error) => {
      console.error("stats page render failed", error);
      setText("hero-note", "stats are temporarily unavailable, try again in a minute");
      startLiveStars(0);
    });
}
