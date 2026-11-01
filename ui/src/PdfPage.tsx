// A page of the document, drawn from the PDF itself by pdf.js, behind the boxes. The library is
// large, so it arrives only when a document opens, and a page is drawn only as it comes into view:
// a long contract costs what the reader looks at. When a PDF cannot be read here, the sheet stands
// in as it always could, and the PDF is a click away.

import { useEffect, useRef, useState } from "react";
import type { PDFDocumentLoadingTask, PDFDocumentProxy, RenderTask } from "pdfjs-dist";

let library: Promise<typeof import("pdfjs-dist")> | null = null;

function pdfjs() {
  library ??= Promise.all([import("pdfjs-dist"), import("pdfjs-dist/build/pdf.worker.min.mjs?url")]).then(([lib, worker]) => {
    lib.GlobalWorkerOptions.workerSrc = worker.default;
    return lib;
  });
  return library;
}

/** The PDF behind `key`, once `load` has fetched it; null until then, and if it cannot be read. */
export function usePdf(load: (() => Promise<ArrayBuffer>) | null, key: string): PDFDocumentProxy | null {
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const fetcher = useRef(load);
  fetcher.current = load;
  const ready = !!load;
  useEffect(() => {
    if (!ready || !fetcher.current) return;
    let gone = false;
    let task: PDFDocumentLoadingTask | null = null;
    Promise.all([fetcher.current(), pdfjs()])
      .then(([data, lib]) => {
        if (gone) return null;
        task = lib.getDocument({ data });
        return task.promise;
      })
      .then((doc) => {
        if (!gone) setPdf(doc);
      })
      .catch(() => setPdf(null));
    return () => {
      gone = true;
      setPdf(null);
      void task?.destroy();
    };
  }, [key, ready]);
  return pdf;
}

export function PdfPage({ pdf, number }: { pdf: PDFDocumentProxy; number: number }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = canvas.current;
    if (!c) return;
    let task: RenderTask | null = null;
    const draw = async () => {
      const page = await pdf.getPage(number);
      const sharp = Math.min(window.devicePixelRatio || 1, 2);
      const viewport = page.getViewport({ scale: (c.clientWidth / page.getViewport({ scale: 1 }).width) * sharp });
      c.width = Math.floor(viewport.width);
      c.height = Math.floor(viewport.height);
      task = page.render({ canvas: c, viewport });
      await task.promise;
    };
    const seen = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        seen.disconnect();
        // A render that is cancelled or fails leaves the sheet showing.
        draw().catch(() => undefined);
      },
      { rootMargin: "600px" },
    );
    seen.observe(c);
    return () => {
      seen.disconnect();
      task?.cancel();
    };
  }, [pdf, number]);
  return <canvas ref={canvas} className="sg-canvas" aria-hidden="true" />;
}
