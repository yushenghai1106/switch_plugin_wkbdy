import type { ReactNode } from "react";

/**
 * 统计页的分区标题：比卡片标题更小更轻，用于给长页面切分区块。
 *
 * `id` 同时作为页内分区导航（锚点）的目标，键盘与读屏都能定位。
 */
export function SectionTitle({ id, children }: { id: string; children: ReactNode }) {
  return (
    <div className="scroll-mt-6 px-1" id={`${id}-anchor`}>
      <h2 id={id} className="text-[13px] font-medium leading-5">
        {children}
      </h2>
    </div>
  );
}
