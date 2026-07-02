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
          muted:      "#878a98",
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
