// Where a box sits on a page. Fields are kept in points from the top left of the page as it is
// displayed (the server's units); a sheet draws them as shares of its own width and height, so
// one box follows the sheet whatever size it is shown at.

export interface Size {
  width: number;
  height: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

const share = (v: number, of: number) => `${((v / of) * 100).toFixed(3)}%`;

/** A box's place on its sheet, as styles. `--fs` is the size its text would have at the page's own
 *  scale (a share of the sheet's width), which the sheet's styles keep readable. */
export function placement(r: Rect, page: Size): Record<string, string> {
  return {
    left: share(r.x, page.width),
    top: share(r.y, page.height),
    width: share(r.width, page.width),
    height: share(r.height, page.height),
    "--fs": `${((Math.min(r.height * 0.6, 14) / page.width) * 100).toFixed(3)}cqw`,
  };
}
