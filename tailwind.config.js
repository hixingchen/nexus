/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        nexus: {
          titlebar:  "#3b414d",   // title_bar.background (zed)
          bg:        "#282c33",
          editor:    "#282c33",
          surface:   "#2f343e",   // panel.background (zed)
          hover:     "#363c46",   // element.hover
          selected:  "#454a56",   // element.selected
          border:    "#464b57",   // border
          accent:    "#74ade8",   // accent
          "accent-hover": "#5a95d4",
          text:       "#dce0e5",
          "text-muted":"#a9afbc",
          // UX-12：原值 #878a98 在 surface #2f343e 上仅 3.64:1、在 bg #282c33 上 4.08:1，
          // 而它承载的是 11–13px 正文（小字号要求 ≥4.5:1）。#9aa0ad 在 surface 上 4.76:1，达标
          muted:      "#9aa0ad",
          success:    "#98c379",
          warning:    "#e5c07b",
          error:      "#e06c75",
          info:       "#56b6c2",
        },
      },
    },
  },
  plugins: [],
};
