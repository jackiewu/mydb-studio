import { linter, type Diagnostic } from "@codemirror/lint";
import { api, type Finding } from "../api";
import { statementRanges } from "./statements";

const SEVERITY: Record<Finding["level"], Diagnostic["severity"]> = {
  danger: "error",
  warn: "warning",
  info: "info",
};

/**
 * Underlines statements the pre-run checks flag while typing: red for
 * ones that can destroy data, yellow for likely slow ones. Hovering shows
 * why and what to do instead.
 */
export function advisorLint(keep: () => (f: Finding) => boolean) {
  return linter(
    async (view) => {
      const doc = view.state.doc.toString();
      // The server-side splitter treats DELIMITER blocks differently.
      if (!doc.trim() || /^\s*delimiter\s/im.test(doc)) return [];
      const findings = await api.checkSql(doc).catch(() => [] as Finding[]);
      if (view.state.doc.toString() !== doc) return [];
      const ranges = statementRanges(doc);
      return findings.filter(keep()).flatMap((f): Diagnostic[] => {
        const r = ranges[f.statement];
        if (!r) return [];
        return [
          {
            from: r.from,
            to: r.to,
            severity: SEVERITY[f.level],
            source: f.level === "danger" ? "危險" : f.level === "warn" ? "效能" : "提示",
            message: `${f.message}\n建議：${f.suggestion}`,
          },
        ];
      });
    },
    { delay: 600 },
  );
}
