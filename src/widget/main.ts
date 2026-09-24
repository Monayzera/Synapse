import { mount } from "svelte";
import "../lib/theme.css";
import { initTheme } from "../lib/theme";
import Widget from "./Widget.svelte";

initTheme();

const target = document.getElementById("app");
if (target) {
  mount(Widget, { target });
}
