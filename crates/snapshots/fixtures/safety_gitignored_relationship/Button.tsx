import styles from './Button.module.css';
export const Label = () => <span className={styles.label} />;
export const Button = () => <button className={styles.button}><Label /></button>;
