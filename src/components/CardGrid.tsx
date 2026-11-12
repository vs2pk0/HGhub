import { useLayoutEffect, useRef, useState, type ReactNode } from "react";

/** Size from the actual content pane, including sidebars, split views and rotation. */
export function CardGrid({ children, gap = 4 }: { children: ReactNode; gap?: number }) {
  const host = useRef<HTMLDivElement>(null);
  const [columns, setColumns] = useState(2);
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return;
    const update = (width: number) => {
      // Phones keep two readable covers; larger panes target ~180px per cover.
      setColumns(width <= 480 ? 2 : Math.max(2, Math.floor((width + gap) / (180 + gap))));
    };
    update(el.clientWidth);
    const observer = new ResizeObserver(entries => update(entries[0].contentRect.width));
    observer.observe(el);
    return () => observer.disconnect();
  }, [gap]);
  return <div ref={host} className="grid min-w-0" style={{ gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`, gap }}>
    {children}
  </div>;
}
