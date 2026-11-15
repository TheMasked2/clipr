import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';
import { loadAccent } from './lib/accent';

loadAccent();

export default mount(App, { target: document.getElementById('app')! });
