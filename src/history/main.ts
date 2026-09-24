import { mount } from "svelte";
import "../lib/theme.css";
import { initTheme } from "../lib/theme";
import History from "./History.svelte";

initTheme();

const target = document.getElementById("app");
if (target) {
  mount(History, { target });
}
