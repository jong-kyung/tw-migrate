import './Button.css';

export function Button() {
  return (
    <section>
      <h1>Preview context</h1>
      <p>Before first button</p>
      <p>First description</p>
      <button className="button">First</button>
      <p>After first button</p>
      <p>Second description</p>
      <p>Third description</p>
      <p>Omitted middle context</p>
      <p>Fourth description</p>
      <p>Fifth description</p>
      <p>Before second button</p>
      <button className="button">Second</button>
      <p>After second button</p>
      <p>Final description</p>
    </section>
  );
}
