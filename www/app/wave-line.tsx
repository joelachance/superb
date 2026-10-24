"use client";

import { useEffect, useRef } from "react";

export function WaveLine({
  className,
  height = 40,
}: {
  className?: string;
  height?: number;
}) {
  const svgRef = useRef<SVGSVGElement>(null);
  const pathRef = useRef<SVGPathElement>(null);

  useEffect(() => {
    const svg = svgRef.current;
    const path = pathRef.current;
    if (!svg || !path) return;

    const mid = height / 2;
    const peak = height * 0.46;
    const smallAmp = 2.6;
    // The large wave falls off quickly. The small sine is the long part:
    // it holds while the pointer stays, then rings out on a slow decay.
    const dropMs = 280;
    const tailMs = 4200;
    let width = 0;
    let phase = 0;
    let surge = 0;
    let ring = 0;
    let over = false;
    let lastTime = performance.now();
    let frame = 0;
    let running = false;
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;

    const flat = () => {
      path.setAttribute("d", `M0 ${mid} H${width}`);
    };

    const resize = () => {
      width = svg.clientWidth;
      svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
      if (!running) flat();
    };

    const draw = (now: number) => {
      const dt = Math.min(Math.max(now - lastTime, 1), 50);
      lastTime = now;

      if (!reduced) {
        surge *= Math.exp(-dt / dropMs);
        if (!over) ring *= Math.exp(-dt / tailMs);
      }

      const amplitude = reduced
        ? 0
        : smallAmp * ring + (peak - smallAmp) * surge;

      if (width < 2 || amplitude < 0.08) {
        flat();
      } else {
        const wavelength = 70 + surge * 210;
        const speed = 0.62 - surge * 0.48;
        phase += speed * (dt / 16.67);
        const frequency = (Math.PI * 2) / wavelength;
        let d = `M0 ${mid.toFixed(2)}`;
        for (let x = 2; x < width; x += 2) {
          const envelope = Math.sin((Math.PI * x) / width);
          const y =
            mid + Math.sin(x * frequency + phase) * amplitude * envelope;
          d += `L${x} ${y.toFixed(2)}`;
        }
        d += `L${width} ${mid.toFixed(2)}`;
        path.setAttribute("d", d);
      }

      if (over || surge > 0.012 || ring > 0.035) {
        frame = requestAnimationFrame(draw);
      } else {
        running = false;
        flat();
      }
    };

    const start = () => {
      if (running) return;
      running = true;
      lastTime = performance.now();
      frame = requestAnimationFrame(draw);
    };

    const enter = () => {
      if (reduced) return;
      over = true;
      surge = 1;
      ring = 1;
      start();
    };

    const leave = () => {
      over = false;
    };

    resize();
    svg.addEventListener("pointerenter", enter);
    svg.addEventListener("pointerleave", leave);
    window.addEventListener("resize", resize);

    return () => {
      cancelAnimationFrame(frame);
      svg.removeEventListener("pointerenter", enter);
      svg.removeEventListener("pointerleave", leave);
      window.removeEventListener("resize", resize);
    };
  }, [height]);

  return (
    <svg
      ref={svgRef}
      className={className}
      height={height}
      aria-hidden="true"
    >
      <rect width="100%" height="100%" fill="transparent" />
      <path
        ref={pathRef}
        d={`M0 ${height / 2} H2000`}
        fill="none"
        stroke="#000"
        strokeWidth="1"
      />
    </svg>
  );
}
