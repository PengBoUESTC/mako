import {foo} from "demo-pkg"
import "./require-demo-pkg"
console.log('unused import')
// This causes "index-main.js" to be selected
require('demo-pkg')