// A page of the document as a sheet of its own proportions, for boxes to sit on. The page's picture
// (PdfPage) goes behind them; without one, the sheet still shows where things go on the page.

import type { CSSProperties, ReactNode, Ref } from "react";
import { placement, type Rect, type Size } from "./geometry";

/** The style that puts a box where its points say on a sheet of this page. */
export const boxStyle = (r: Rect, page: Size) => placement(r, page) as CSSProperties;

export function Sheet({
  number,
  count,
  size,
  children,
  inner,
  note,
  className,
  picture,
}: {
  number: number;
  count: number;
  size: Size;
  children?: ReactNode;
  /** The sheet itself, to measure it. */
  inner?: Ref<HTMLDivElement>;
  /** Beside the page's number: how many boxes it holds, say. */
  note?: ReactNode;
  className?: string;
  /** The page itself, behind the boxes. */
  picture?: ReactNode;
}) {
  return (
    <section className="sg-page" id={`page-${number}`} aria-label={`Page ${number} of ${count}`}>
      <header className="sg-page-head">
        <span>
          Page {number} of {count}
        </span>
        {note && <span>{note}</span>}
      </header>
      <div ref={inner} data-sheet={number} className={`sg-sheet ${className ?? ""}`} style={{ aspectRatio: `${size.width} / ${size.height}` }}>
        {picture}
        {children}
      </div>
    </section>
  );
}
