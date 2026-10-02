import type { LucideIcon } from "lucide-react";

/**
 * 统计指标卡：图标 + 标签 + 数值。
 *
 * 积分统计与 Token 统计的「总览」区此前各持一份**逐字相同**的实现，这里收敛为
 * 单一组件；`divided` 用于同一行内给非首列加分隔线。
 */
export function StatMetric({
  icon: Icon,
  label,
  value,
  divided = false,
  hint,
}: {
  icon: LucideIcon;
  label: string;
  value: string;
  divided?: boolean;
  /** 可选的补充说明（如「7 天内到期」的时间点），悬停可见。 */
  hint?: string;
}) {
  return (
    <div
      className={`flex min-w-0 flex-col items-center justify-center px-4 py-5 text-center sm:py-3 ${
        divided ? "sm:border-l sm:border-border/60" : ""
      }`}
      title={hint}
    >
      <div className="flex max-w-full items-center justify-center gap-2 text-[13px] font-medium leading-5 text-muted-foreground">
        <Icon className="size-4 shrink-0 stroke-[1.75]" aria-hidden="true" />
        <span className="truncate">{label}</span>
      </div>
      <div
        className="mt-3 max-w-full truncate text-[26px] font-semibold leading-8 tracking-[-0.025em] text-foreground tabular-nums"
        style={{
          fontFamily:
            '"Bricolage Grotesque Variable", "SF Pro Display", ui-sans-serif, sans-serif',
        }}
      >
        {value}
      </div>
    </div>
  );
}
