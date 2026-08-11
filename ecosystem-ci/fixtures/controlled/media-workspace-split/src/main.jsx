import React from 'react';
import { createRoot } from 'react-dom/client';
import Card from '../packages/app/src/Card.jsx';
import Badge from '../packages/lib/src/Badge.jsx';

createRoot(document.getElementById('root')).render(<main>
  <Card />
  <Badge />
  <button data-identity="ready">Ready</button>
</main>);
