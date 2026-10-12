"use client";

import { useEffect, useRef } from "react";

const HEIGHT = 160;
const MID = HEIGHT / 2;

export function WaveLine() {
  const svgRef = useRef<SVGSVGElement>(null);
  const pathRef = useRef<SVGPathElement>(null);

  useEffect(() => {
    const svg = svgRef.current;
    const path = pathRef.current;
    if (!svg || !path) return;

    let width = 0;
    let mouseY = Number.POSITIVE_INFINITY;
    let phase = 0;
    let farMix = 1;
    let lastTime = performance.now();
    let frame = 0;

    const resize = () => {
      width = svg.clientWidth;
      svg.setAttribute("viewBox", `0 0 ${width} ${HEIGHT}`);
    };

    const onMove = (event: PointerEvent) => {
      mouseY = event.clientY;
    };

    const onOut = (event: PointerEvent) => {
      if (!event.relatedTarget) mouseY = Number.POSITIVE_INFINITY;
    };

    const draw = (now: number) => {
      const dt = Math.min(Math.max(now - lastTime, 1), 50);
      lastTime = now;

      const lineY = window.innerHeight / 2;
      const distance = Math.abs(mouseY - lineY);
      const far = Math.min(distance / (window.innerHeight * 0.42), 1);
      farMix += (far - farMix) * 0.18;

      const closeness = (1 - farMix) ** 2;
      const amplitude = 0.2 + closeness * 55.8;
      const wavelength = 320;
      const speed = 0.1 + farMix * 1.05;
      phase += speed * (dt / 16.67);

      const frequency = (Math.PI * 2) / wavelength;
      let d = `M0 ${MID.toFixed(2)}`;
      for (let x = 2; x < width; x += 2) {
        const envelope = Math.sin((Math.PI * x) / width);
        const y =
          MID + Math.sin(x * frequency + phase) * amplitude * envelope;
        d += `L${x} ${y.toFixed(2)}`;
      }
      d += `L${width} ${MID.toFixed(2)}`;
      path.setAttribute("d", d);
      frame = requestAnimationFrame(draw);
    };

    resize();
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerout", onOut);
    window.addEventListener("resize", resize);
    frame = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerout", onOut);
      window.removeEventListener("resize", resize);
    };
  }, []);

  return (
    <svg
      ref={svgRef}
      className="pointer-events-none fixed inset-x-0 top-1/2 h-40 w-full -translate-y-1/2"
      aria-hidden="true"
    >
      <path
        ref={pathRef}
        d={`M0 ${MID} H2000`}
        fill="none"
        stroke="#000"
        strokeWidth="1"
      />
    </svg>
  );
}
