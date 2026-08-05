import styles from './Card.module.css';

export const Card = ({ active, ready, variant }: { active: boolean; ready: boolean; variant: string }) => (
  <div className={active ? styles.active : styles.inactive}>
    <span className={ready && 'card'} />
    <span className={styles.enabled ? 'card' : getClass(variant)} />
  </div>
);
